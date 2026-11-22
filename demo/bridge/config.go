package main

import (
	"os"
	"strconv"
	"strings"
	"time"
)

// config is everything the bridge takes from its environment. Defaults are
// production-shaped; tests build a config directly.
type config struct {
	Addr        string
	TmpDir      string
	Bin         string
	SeedDir     string
	Provider    string
	Model       string
	ProvidedKey string
	Dev         string

	MaxSessions int
	HourlyPerIP int
	DailyTotal  int
	SessionMax  time.Duration
	IdleTimeout time.Duration

	Origins   []string
	ReadLimit int64
}

func loadConfig() config {
	return config{
		Addr:        env("ADDR", "127.0.0.1:8080"),
		TmpDir:      env("YOKE_DEMO_TMP", "/tmp/yoke-demo"),
		Bin:         env("YOKE_DEMO_BIN", "/yoke"),
		SeedDir:     findSeedDir(),
		Provider:    env("YOKE_DEMO_PROVIDER", "groq"),
		Model:       env("YOKE_DEMO_MODEL", "qwen/qwen3.8-27b"),
		ProvidedKey: os.Getenv("GROQ_API_KEY"),
		Dev:         os.Getenv("YOKE_DEV"),
		MaxSessions: envInt("YOKE_DEMO_MAX_SESSIONS", 6),
		HourlyPerIP: envInt("YOKE_DEMO_HOURLY_PER_IP", 3),
		DailyTotal:  envInt("YOKE_DEMO_DAILY_TOTAL", 150),
		SessionMax:  envMinutes("YOKE_DEMO_SESSION_MINUTES", 15),
		IdleTimeout: envMinutes("YOKE_DEMO_IDLE_MINUTES", 3),
		Origins:     splitEnv("YOKE_DEMO_ORIGINS", "https://yoke.santa.build,http://localhost,http://127.0.0.1"),
		ReadLimit:   64 << 10,
	}
}

func env(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func envInt(key string, def int) int {
	v, err := strconv.Atoi(strings.TrimSpace(os.Getenv(key)))
	if err != nil || v < 0 {
		return def
	}
	return v
}

func envMinutes(key string, def int) time.Duration {
	return time.Duration(envInt(key, def)) * time.Minute
}

func splitEnv(key, def string) []string {
	out := []string{}
	for _, part := range strings.Split(env(key, def), ",") {
		if p := strings.TrimSpace(part); p != "" {
			out = append(out, p)
		}
	}
	return out
}
