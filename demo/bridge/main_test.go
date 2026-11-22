package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func testConfig(t *testing.T) config {
	t.Helper()
	root := t.TempDir()
	seed := filepath.Join(root, "seed")
	if err := os.MkdirAll(filepath.Join(seed, "src"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(seed, "notes.txt"), []byte("hello seed\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(seed, "src", "main.rs"), []byte("fn main() {}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	fake := filepath.Join(root, "fake.sh")
	if err := os.WriteFile(fake, []byte("#!/bin/sh\nprintf 'fake child ready\\n'\nexec /bin/cat\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	return config{
		TmpDir:      filepath.Join(root, "sessions"),
		Bin:         fake,
		SeedDir:     seed,
		Provider:    "mock",
		Model:       "mock",
		ProvidedKey: "demo-key",
		MaxSessions: 2,
		HourlyPerIP: 3,
		DailyTotal:  10,
		SessionMax:  2 * time.Minute,
		IdleTimeout: time.Minute,
		Origins:     []string{"http://localhost"},
		ReadLimit:   64 << 10,
	}
}

func TestClientIP(t *testing.T) {
	cases := []struct {
		name    string
		headers map[string]string
		remote  string
		want    string
	}{
		{"cloudflare", map[string]string{"CF-Connecting-IP": "203.0.113.9", "X-Forwarded-For": "198.51.100.1"}, "10.0.0.1:1234", "203.0.113.9"},
		{"x-forwarded-first", map[string]string{"X-Forwarded-For": "198.51.100.1, 10.0.0.2"}, "10.0.0.1:1234", "198.51.100.1"},
		{"peer", nil, "198.51.100.7:9000", "198.51.100.7"},
		{"bad header falls through", map[string]string{"CF-Connecting-IP": "not-an-ip"}, "198.51.100.2:1", "198.51.100.2"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest("GET", "/ws", nil)
			r.RemoteAddr = tc.remote
			for k, v := range tc.headers {
				r.Header.Set(k, v)
			}
			if got := clientIP(r); got != tc.want {
				t.Fatalf("clientIP = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestOriginAllowed(t *testing.T) {
	allowed := []string{"https://yoke.santa.build", "http://localhost", "http://127.0.0.1"}
	cases := map[string]bool{
		"":                              true, // non-browser client
		"https://yoke.santa.build":      true,
		"http://localhost:5173":         true,
		"http://127.0.0.1:8080":         true,
		"https://evil.example":          false,
		"https://yoke.santa.build.evil": false,
		"http://yoke.santa.build":       false,
		"ftp://localhost":               false,
	}
	for origin, want := range cases {
		if got := originAllowed(allowed, origin); got != want {
			t.Errorf("originAllowed(%q) = %v, want %v", origin, got, want)
		}
	}
}

func TestLimiterHourlyAndDaily(t *testing.T) {
	now := time.Date(2026, 11, 22, 12, 0, 0, 0, time.UTC)
	l := newLimiter(2, 3)
	if ok, _, _ := l.take("1.2.3.4", now); !ok {
		t.Fatal("first session refused")
	}
	if ok, _, _ := l.take("1.2.3.4", now.Add(time.Minute)); !ok {
		t.Fatal("second session refused")
	}
	ok, wait, msg := l.take("1.2.3.4", now.Add(2*time.Minute))
	if ok || wait < 50*time.Minute || !strings.Contains(msg, "own Groq key") {
		t.Fatalf("third session should be limited, got ok=%v wait=%v msg=%q", ok, wait, msg)
	}
	// A different visitor gets the last daily slot.
	if ok, _, _ := l.take("5.6.7.8", now.Add(2*time.Minute)); !ok {
		t.Fatal("other visitor refused before daily cap")
	}
	ok, wait, msg = l.take("9.9.9.9", now.Add(2*time.Minute))
	if ok || wait <= 0 || !strings.Contains(msg, "tomorrow") {
		t.Fatalf("daily cap not enforced, ok=%v wait=%v msg=%q", ok, wait, msg)
	}
	// The next UTC day starts fresh.
	if ok, _, _ := l.take("9.9.9.9", now.Add(24*time.Hour)); !ok {
		t.Fatal("daily limit did not reset")
	}
}

func TestValidateStart(t *testing.T) {
	good := startMsg{Type: "start", Cols: 80, Rows: 24}
	if err := validateStart(good); err != nil {
		t.Fatal(err)
	}
	bad := []startMsg{
		{Type: "hello", Cols: 80, Rows: 24},
		{Type: "start", Cols: 0, Rows: 24},
		{Type: "start", Cols: 80, Rows: 0},
		{Type: "start", Cols: 80, Rows: 24, Key: "abc\nKEY=leak"},
		{Type: "start", Cols: 80, Rows: 24, Key: strings.Repeat("x", 257)},
	}
	for i, m := range bad {
		if validateStart(m) == nil {
			t.Errorf("bad start %d was accepted", i)
		}
	}
}

func TestCopySeed(t *testing.T) {
	cfg := testConfig(t)
	dst := t.TempDir()
	if err := copySeed(cfg.SeedDir, dst); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(filepath.Join(dst, "src", "main.rs"))
	if err != nil || string(b) != "fn main() {}\n" {
		t.Fatalf("seed file missing or changed: %q, %v", b, err)
	}
}

func TestSweepStale(t *testing.T) {
	tmp := filepath.Join(t.TempDir(), "sessions")
	if err := os.MkdirAll(filepath.Join(tmp, "old-session"), 0o700); err != nil {
		t.Fatal(err)
	}
	n, err := sweepStale(tmp)
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("swept %d, want 1", n)
	}
	entries, _ := os.ReadDir(tmp)
	if len(entries) != 0 {
		t.Fatalf("tmp not empty after sweep: %v", entries)
	}
}

func TestSessionEndToEnd(t *testing.T) {
	cfg := testConfig(t)
	srv, err := newServer(cfg)
	if err != nil {
		t.Fatal(err)
	}
	ts := httptest.NewServer(srv.routes())
	defer ts.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	url := "ws" + strings.TrimPrefix(ts.URL, "http") + "/ws"
	conn, _, err := websocket.Dial(ctx, url, &websocket.DialOptions{
		HTTPHeader: http.Header{"Origin": []string{"http://localhost:5173"}},
	})
	if err != nil {
		t.Fatal(err)
	}
	conn.SetReadLimit(1 << 20)
	defer conn.CloseNow()

	start, _ := json.Marshal(startMsg{Type: "start", Cols: 80, Rows: 24})
	if err := conn.Write(ctx, websocket.MessageText, start); err != nil {
		t.Fatal(err)
	}

	if got := readUntil(ctx, t, conn, "fake child ready"); !strings.Contains(got, "fake child ready") {
		t.Fatalf("missing child output, got %q", got)
	}
	if err := conn.Write(ctx, websocket.MessageBinary, []byte("hello pty\n")); err != nil {
		t.Fatal(err)
	}
	if got := readUntil(ctx, t, conn, "hello pty"); !strings.Contains(got, "hello pty") {
		t.Fatalf("PTY echo missing, got %q", got)
	}

	entries, err := os.ReadDir(cfg.TmpDir)
	if err != nil || len(entries) != 1 {
		t.Fatalf("expected one session dir, got %v (%v)", entries, err)
	}
	dir := filepath.Join(cfg.TmpDir, entries[0].Name())
	if _, err := os.Stat(filepath.Join(dir, "work", "notes.txt")); err != nil {
		t.Fatalf("seed not copied: %v", err)
	}

	if err := conn.Close(websocket.StatusNormalClosure, "done"); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(cfg.TmpDir); os.IsNotExist(err) {
			break
		}
		entries, _ := os.ReadDir(cfg.TmpDir)
		if len(entries) == 0 {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if entries, _ := os.ReadDir(cfg.TmpDir); len(entries) != 0 {
		t.Fatalf("session dir not removed: %v", entries)
	}
}

func TestProvidedTokenLimitOverWS(t *testing.T) {
	cfg := testConfig(t)
	cfg.HourlyPerIP = 1
	srv, err := newServer(cfg)
	if err != nil {
		t.Fatal(err)
	}
	ts := httptest.NewServer(srv.routes())
	defer ts.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	url := "ws" + strings.TrimPrefix(ts.URL, "http") + "/ws"

	open := func() *websocket.Conn {
		c, _, err := websocket.Dial(ctx, url, nil)
		if err != nil {
			t.Fatal(err)
		}
		start, _ := json.Marshal(startMsg{Type: "start", Cols: 80, Rows: 24})
		if err := c.Write(ctx, websocket.MessageText, start); err != nil {
			t.Fatal(err)
		}
		return c
	}

	first := open()
	readUntil(ctx, t, first, "fake child ready")
	first.Close(websocket.StatusNormalClosure, "done")

	// Wait for the first session to release its slot.
	time.Sleep(200 * time.Millisecond)
	second := open()
	defer second.CloseNow()
	_, data, err := second.Read(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "\"type\":\"limit\"") {
		t.Fatalf("expected limit frame, got %s", data)
	}
}

func readUntil(ctx context.Context, t *testing.T, conn *websocket.Conn, needle string) string {
	t.Helper()
	var seen strings.Builder
	for {
		typ, data, err := conn.Read(ctx)
		if err != nil {
			t.Fatalf("read while waiting for %q: %v (seen %q)", needle, err, seen.String())
		}
		if typ == websocket.MessageBinary {
			seen.Write(data)
			if strings.Contains(seen.String(), needle) {
				return seen.String()
			}
		}
	}
}
