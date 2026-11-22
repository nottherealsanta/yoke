// yoke-demo/bridge serves https://yoke.santa.build: the static page plus one
// WebSocket per visitor that bridges to a PTY running `yoke` in a throwaway
// directory. The visitor's browser is the terminal; this process is only the
// glass between it and the child.
package main

import (
	"context"
	"embed"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/coder/websocket"
)

//go:embed all:web/dist
var distFS embed.FS

type startMsg struct {
	Type string `json:"type"`
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
	Key  string `json:"key"`
}

type resizeMsg struct {
	Type string `json:"type"`
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
}

type statusMsg struct {
	Type       string `json:"type"`
	Msg        string `json:"msg,omitempty"`
	Reason     string `json:"reason,omitempty"`
	RetryAfter int    `json:"retry_after,omitempty"`
	Limit      int    `json:"limit_seconds,omitempty"`
}

type server struct {
	cfg     config
	dist    fs.FS
	limits  *limiter
	active  atomic.Int32
	started time.Time
}

func newServer(cfg config) (*server, error) {
	sub, err := fs.Sub(distFS, "web/dist")
	if err != nil {
		return nil, err
	}
	return &server{cfg: cfg, dist: sub, limits: newLimiter(cfg.HourlyPerIP, cfg.DailyTotal), started: time.Now()}, nil
}

func (s *server) routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/ws", s.handleWS)
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		io.WriteString(w, "ok\n")
	})
	files := http.FileServer(http.FS(s.dist))
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("Cache-Control", "no-store")
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Content-Security-Policy", "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
		files.ServeHTTP(w, r)
	})
	return mux
}

func (s *server) handleWS(w http.ResponseWriter, r *http.Request) {
	if !originAllowed(s.cfg.Origins, r.Header.Get("Origin")) {
		http.Error(w, "origin not allowed", http.StatusForbidden)
		return
	}
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{InsecureSkipVerify: true})
	if err != nil {
		return
	}
	defer conn.CloseNow()
	conn.SetReadLimit(s.cfg.ReadLimit)

	if n := s.active.Add(1); int(n) > s.cfg.MaxSessions {
		s.active.Add(-1)
		s.send(conn, statusMsg{Type: "status", Msg: "The demo is busy right now. Try again in a minute — bringing your own key doesn't bypass capacity."})
		conn.Close(websocket.StatusTryAgainLater, "busy")
		return
	}
	defer s.active.Add(-1)

	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()

	readCtx, stop := context.WithTimeout(ctx, 15*time.Second)
	typ, data, err := conn.Read(readCtx)
	stop()
	if err != nil || typ != websocket.MessageText {
		s.send(conn, statusMsg{Type: "status", Msg: "Expected a start message."})
		conn.Close(websocket.StatusPolicyViolation, "expected start")
		return
	}
	var start startMsg
	if err := json.Unmarshal(data, &start); err != nil {
		s.send(conn, statusMsg{Type: "status", Msg: "Could not read the start message."})
		conn.Close(websocket.StatusPolicyViolation, "bad start")
		return
	}
	if err := validateStart(start); err != nil {
		s.send(conn, statusMsg{Type: "status", Msg: err.Error()})
		conn.Close(websocket.StatusPolicyViolation, "bad start")
		return
	}

	key := strings.TrimSpace(start.Key)
	provided := key == ""
	if provided {
		key = s.cfg.ProvidedKey
		if key == "" {
			s.send(conn, statusMsg{Type: "status", Msg: "Free demo tokens are not available right now. Bring your own Groq key to start a session."})
			conn.Close(websocket.StatusServiceRestart, "no demo key")
			return
		}
		ok, retry, msg := s.limits.take(clientIP(r), time.Now())
		if !ok {
			s.send(conn, statusMsg{Type: "limit", Msg: msg, RetryAfter: int(retry.Seconds() + 0.5)})
			conn.Close(websocket.StatusTryAgainLater, "limit")
			return
		}
	}

	sess, err := startSession(s.cfg, key, start.Cols, start.Rows)
	if err != nil {
		log.Printf("session start failed: %v", err)
		s.send(conn, statusMsg{Type: "status", Msg: "The demo could not start. Please try again in a minute."})
		conn.Close(websocket.StatusInternalError, "spawn failed")
		return
	}
	defer sess.close()

	s.send(conn, statusMsg{Type: "ready", Limit: int(s.cfg.SessionMax.Seconds())})
	started := time.Now()
	var last atomic.Int64
	last.Store(started.UnixNano())

	var writeMu sync.Mutex
	write := func(kind websocket.MessageType, b []byte) error {
		writeMu.Lock()
		defer writeMu.Unlock()
		wctx, wc := context.WithTimeout(context.Background(), 10*time.Second)
		defer wc()
		return conn.Write(wctx, kind, b)
	}

	// PTY → browser.
	go func() {
		defer cancel()
		buf := make([]byte, 32<<10)
		for {
			n, err := sess.ptmx.Read(buf)
			if n > 0 {
				if write(websocket.MessageBinary, append([]byte(nil), buf[:n]...)) != nil {
					return
				}
			}
			if err != nil {
				return
			}
		}
	}()

	// Browser → PTY.
	go func() {
		defer cancel()
		for {
			typ, data, err := conn.Read(ctx)
			if err != nil {
				return
			}
			last.Store(time.Now().UnixNano())
			if typ == websocket.MessageBinary {
				if _, err := sess.ptmx.Write(data); err != nil {
					return
				}
				continue
			}
			var m resizeMsg
			if json.Unmarshal(data, &m) == nil && m.Type == "resize" && m.Cols > 0 && m.Rows > 0 {
				_ = ptySetsize(sess, m.Cols, m.Rows)
			}
		}
	}()

	// Timers: wall clock and inactivity. Both end the session the same way.
	go func() {
		t := time.NewTicker(time.Second)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case now := <-t.C:
				var msg string
				if now.Sub(started) > s.cfg.SessionMax {
					msg = "This demo session reached its time limit. Start a new one whenever you like."
				} else if now.Sub(time.Unix(0, last.Load())) > s.cfg.IdleTimeout {
					msg = "This demo session ended after a few quiet minutes. Start a new one whenever you like."
				}
				if msg != "" {
					s.send(conn, statusMsg{Type: "end", Reason: msg})
					cancel()
					return
				}
			}
		}
	}()

	<-ctx.Done()
	conn.Close(websocket.StatusNormalClosure, "session ended")
}

func (s *server) send(conn *websocket.Conn, m statusMsg) {
	b, err := json.Marshal(m)
	if err != nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = conn.Write(ctx, websocket.MessageText, b)
}

func validateStart(m startMsg) error {
	if m.Type != "start" {
		return fmt.Errorf("expected a start message")
	}
	if m.Cols < 20 || m.Cols > 500 || m.Rows < 5 || m.Rows > 300 {
		return fmt.Errorf("terminal size is out of range")
	}
	if len(m.Key) > 256 || strings.ContainsAny(m.Key, "\x00\r\n") {
		return fmt.Errorf("that key doesn't look right")
	}
	return nil
}

func main() {
	healthcheck := flag.Bool("healthcheck", false, "check /healthz and exit")
	flag.Parse()

	cfg := loadConfig()
	if *healthcheck {
		os.Exit(runHealthcheck(cfg.Addr))
	}
	if n, err := sweepStale(cfg.TmpDir); err != nil {
		log.Fatalf("prepare %s: %v", cfg.TmpDir, err)
	} else if n > 0 {
		log.Printf("removed %d stale session dirs", n)
	}

	srv, err := newServer(cfg)
	if err != nil {
		log.Fatal(err)
	}
	httpSrv := &http.Server{
		Addr:              cfg.Addr,
		Handler:           srv.routes(),
		ReadHeaderTimeout: 10 * time.Second,
		IdleTimeout:       75 * time.Second,
	}
	go func() {
		sig := make(chan os.Signal, 1)
		signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
		<-sig
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		httpSrv.Shutdown(ctx)
	}()
	log.Printf("yoke demo listening on %s (max %d sessions)", cfg.Addr, cfg.MaxSessions)
	if err := httpSrv.ListenAndServe(); err != http.ErrServerClosed {
		log.Fatal(err)
	}
}

func runHealthcheck(addr string) int {
	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return 1
	}
	if host == "" || host == "0.0.0.0" || host == "::" {
		host = "127.0.0.1"
	}
	client := &http.Client{Timeout: 3 * time.Second}
	resp, err := client.Get("http://" + net.JoinHostPort(host, port) + "/healthz")
	if err != nil {
		return 1
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return 1
	}
	return 0
}
