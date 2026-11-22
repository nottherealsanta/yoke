package main

import (
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

// limiter counts only packages on our tokens. A visitor with their own key is
// never counted, so they can always start a session when capacity allows.
type limiter struct {
	mu     sync.Mutex
	perIP  map[string][]time.Time
	day    string
	dayCnt int
	hourly int
	daily  int
}

func newLimiter(hourlyPerIP, dailyTotal int) *limiter {
	return &limiter{perIP: map[string][]time.Time{}, hourly: hourlyPerIP, daily: dailyTotal}
}

// take records one provided-token session. When refused it returns how long
// the visitor should wait before trying again.
func (l *limiter) take(ip string, now time.Time) (ok bool, retryAfter time.Duration, msg string) {
	l.mu.Lock()
	defer l.mu.Unlock()

	since := now.Add(-time.Hour)
	recent := l.perIP[ip][:0]
	for _, t := range l.perIP[ip] {
		if t.After(since) {
			recent = append(recent, t)
		}
	}
	l.perIP[ip] = recent

	if l.hourly > 0 && len(recent) >= l.hourly {
		wait := recent[0].Add(time.Hour).Sub(now)
		return false, wait, "Free demo tokens are limited to " + itoa(l.hourly) + " sessions per visitor per hour. Bring your own Groq key for unlimited sessions, or try again later."
	}

	day := now.UTC().Format("2006-01-02")
	if l.day != day {
		l.day, l.dayCnt = day, 0
	}
	if l.daily > 0 && l.dayCnt >= l.daily {
		next := time.Date(now.UTC().Year(), now.UTC().Month(), now.UTC().Day(), 0, 0, 0, 0, time.UTC).Add(24 * time.Hour)
		return false, next.Sub(now), "Today's free demo tokens are used up. Bring your own Groq key to keep going, or come back tomorrow."
	}

	l.perIP[ip] = append(recent, now)
	l.dayCnt++
	return true, 0, ""
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var b [20]byte
	i := len(b)
	for n > 0 {
		i--
		b[i] = byte('0' + n%10)
		n /= 10
	}
	return string(b[i:])
}

// clientIP trusts the proxies in front of the bridge: Cloudflare first, then
// the first X-Forwarded-For hop, then the socket peer.
func clientIP(r *http.Request) string {
	if v := strings.TrimSpace(r.Header.Get("CF-Connecting-IP")); v != "" {
		if net.ParseIP(v) != nil {
			return v
		}
	}
	if v := r.Header.Get("X-Forwarded-For"); v != "" {
		first := strings.TrimSpace(strings.Split(v, ",")[0])
		if net.ParseIP(first) != nil {
			return first
		}
	}
	if host, _, err := net.SplitHostPort(r.RemoteAddr); err == nil {
		return host
	}
	return r.RemoteAddr
}

// originAllowed accepts non-browser clients (no Origin) and configured
// origins. A bare localhost entry means any port, for local development.
func originAllowed(origins []string, origin string) bool {
	if origin == "" {
		return true
	}
	u, err := url.Parse(origin)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return false
	}
	for _, allowed := range origins {
		a, err := url.Parse(allowed)
		if err != nil || a.Host == "" {
			continue
		}
		if a.Scheme != "" && a.Scheme != u.Scheme {
			continue
		}
		if a.Port() == "" {
			loopback := a.Hostname() == "localhost" || a.Hostname() == "127.0.0.1"
			if a.Hostname() == u.Hostname() && (loopback || u.Port() == "") {
				return true
			}
			continue
		}
		if strings.EqualFold(a.Host, u.Host) {
			return true
		}
	}
	return false
}
