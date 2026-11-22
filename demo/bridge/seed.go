package main

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// findSeedDir works from the repo (bridge/, demo/ or repo root) and from the
// container, where the workspace template is copied to /workspace-template.
func findSeedDir() string {
	if v := os.Getenv("YOKE_DEMO_SEED"); v != "" {
		return v
	}
	for _, p := range []string{"/workspace-template", "workspace", "../workspace", "demo/workspace"} {
		if st, err := os.Stat(p); err == nil && st.IsDir() {
			return p
		}
	}
	return "../workspace"
}

// copySeed places the sample project in the session's work directory. It only
// handles regular files and directories; the seed is part of the image.
func copySeed(src, dst string) error {
	return filepath.WalkDir(src, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		if strings.HasPrefix(d.Name(), ".") {
			return nil
		}
		target := filepath.Join(dst, rel)
		if d.IsDir() {
			return os.MkdirAll(target, 0o700)
		}
		if !d.Type().IsRegular() {
			return nil
		}
		b, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("read seed %s: %w", rel, err)
		}
		return os.WriteFile(target, b, 0o600)
	})
}
