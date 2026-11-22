package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"syscall"

	"github.com/creack/pty"
)

type session struct {
	cmd  *exec.Cmd
	ptmx *os.File
	dir  string
	once sync.Once
}

// startSession creates the per-visitor scratch directory and starts Yoke in a
// PTY. The child gets a minimal environment: nothing from the bridge leaks in,
// and the API key exists only in this process's environment.
func startSession(cfg config, key string, cols, rows uint16) (*session, error) {
	if err := os.MkdirAll(cfg.TmpDir, 0o700); err != nil {
		return nil, err
	}
	dir, err := os.MkdirTemp(cfg.TmpDir, "session-")
	if err != nil {
		return nil, err
	}
	work := filepath.Join(dir, "work")
	home := filepath.Join(dir, "home")
	if err := os.MkdirAll(work, 0o700); err != nil {
		os.RemoveAll(dir)
		return nil, err
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		os.RemoveAll(dir)
		return nil, err
	}
	if err := copySeed(cfg.SeedDir, work); err != nil {
		os.RemoveAll(dir)
		return nil, err
	}

	bin := cfg.Bin
	if !filepath.IsAbs(bin) {
		if abs, err := filepath.Abs(bin); err == nil {
			bin = abs
		}
	}
	cmd := exec.Command(bin,
		"--provider", cfg.Provider,
		"--model", cfg.Model,
		"--tools", "read,write,edit",
		"--root", work,
		"--no-memory",
		"--yes",
	)
	cmd.Dir = work
	cmd.Env = []string{
		"PATH=/usr/local/bin:/usr/bin:/bin",
		"HOME=" + home,
		"YOKE_HOME=" + home,
		"TERM=xterm-256color",
		"COLORTERM=truecolor",
		"LANG=C.UTF-8",
		"GROQ_API_KEY=" + key,
	}
	if cfg.Dev != "" {
		cmd.Env = append(cmd.Env, "YOKE_DEV="+cfg.Dev)
	}

	ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{Rows: rows, Cols: cols})
	if err != nil {
		os.RemoveAll(dir)
		return nil, fmt.Errorf("start yoke: %w", err)
	}
	return &session{cmd: cmd, ptmx: ptmx, dir: dir}, nil
}

// close kills the whole process group, reaps the child and removes the scratch
// directory. Safe to call more than once.
func (s *session) close() {
	s.once.Do(func() {
		if s.cmd.Process != nil {
			pid := s.cmd.Process.Pid
			_ = syscall.Kill(-pid, syscall.SIGKILL)
			_ = s.cmd.Process.Kill()
			_, _ = s.cmd.Process.Wait()
		}
		_ = s.ptmx.Close()
		_ = os.RemoveAll(s.dir)
	})
}

// sweepStale removes leftovers from a previous run and makes sure the tmp root
// is private. Called once at startup.
func sweepStale(tmp string) (int, error) {
	entries, err := os.ReadDir(tmp)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, os.MkdirAll(tmp, 0o700)
		}
		return 0, err
	}
	n := 0
	for _, e := range entries {
		if err := os.RemoveAll(filepath.Join(tmp, e.Name())); err == nil {
			n++
		}
	}
	return n, nil
}

func ptySetsize(s *session, cols, rows uint16) error {
	return pty.Setsize(s.ptmx, &pty.Winsize{Rows: rows, Cols: cols})
}
