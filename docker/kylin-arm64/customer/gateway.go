// Customer admin entrypoint. Keep the Agent bound to loopback; authenticate the
// external hop before forwarding streaming HTTP and WebSocket requests.
package main

import (
	"crypto/sha256"
	"crypto/subtle"
	"log"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"strings"
	"time"
)

func adminGateway(target *url.URL, password string) http.Handler {
	proxy := httputil.NewSingleHostReverseProxy(target)
	proxy.FlushInterval = -1
	passwordHash := sha256.Sum256([]byte(password))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, supplied, ok := r.BasicAuth()
		suppliedHash := sha256.Sum256([]byte(supplied))
		if !ok || user != "admin" || subtle.ConstantTimeCompare(passwordHash[:], suppliedHash[:]) != 1 {
			w.Header().Set("WWW-Authenticate", `Basic realm="QuickAgent", charset="UTF-8"`)
			w.Header().Set("Cache-Control", "no-store")
			http.Error(w, "QuickAgent administrator login required", http.StatusUnauthorized)
			return
		}
		if strings.EqualFold(r.Header.Get("Sec-Fetch-Site"), "cross-site") {
			http.Error(w, "Cross-site admin requests are not allowed", http.StatusForbidden)
			return
		}
		if raw := r.Header.Get("Origin"); raw != "" {
			origin, err := url.Parse(raw)
			scheme := "http"
			if r.TLS != nil {
				scheme = "https"
			}
			if err != nil || origin.Scheme != scheme || origin.User != nil ||
				origin.Path != "" || origin.RawQuery != "" || origin.Fragment != "" || !strings.EqualFold(origin.Host, r.Host) {
				http.Error(w, "Foreign admin origin is not allowed", http.StatusForbidden)
				return
			}
		}
		if r.URL.Path == "/" {
			http.Redirect(w, r, "/agent/", http.StatusTemporaryRedirect)
			return
		}
		r.Header.Del("Authorization")
		r.Header.Del("Origin")
		r.Host = target.Host
		proxy.ServeHTTP(w, r)
	})
}

func main() {
	password := os.Getenv("CUSTOMER_ADMIN_PASSWORD")
	if len(password) < 12 {
		log.Fatal("CUSTOMER_ADMIN_PASSWORD must contain at least 12 characters")
	}
	target, _ := url.Parse("http://127.0.0.1:3001")
	server := &http.Server{Addr: ":3002", Handler: adminGateway(target, password), ReadHeaderTimeout: 10 * time.Second}
	log.Print("QuickAgent authenticated administrator gateway is ready on port 3002")
	log.Fatal(server.ListenAndServe())
}
