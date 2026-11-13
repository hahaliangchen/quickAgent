package main

import (
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
	"time"
)

func TestAdminGatewayAuthenticationAndLocalForwarding(t *testing.T) {
	const password = "customer-test-password"
	var calls int
	var expectedHost string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if r.Host != expectedHost {
			t.Errorf("forwarded Host %q, want %q", r.Host, expectedHost)
		}
		if r.Header.Get("Authorization") != "" || r.Header.Get("Origin") != "" || r.URL.RawQuery != "check=1" {
			t.Errorf("unexpected forwarded auth/origin/query")
		}
		w.Header().Set("Set-Cookie", "grow_session=test; Path=/; HttpOnly; SameSite=Strict")
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, "data: ready\n\n")
	}))
	defer upstream.Close()
	target, _ := url.Parse(upstream.URL)
	expectedHost = target.Host
	handler := adminGateway(target, password)
	for _, tc := range []struct {
		password, origin, site string
		code                   int
	}{
		{"", "", "", 401}, {"incorrect", "", "", 401},
		{password, "http://customer.example:3001", "same-origin", 200},
		{password, "https://foreign.example", "cross-site", 403},
		{password, "https://foreign.example", "", 403},
		{password, "https://customer.example:3001", "", 403},
		{password, "null", "", 403},
		{password, "http://customer.example:3001/path", "", 403},
		{password, "http://admin@customer.example:3001", "", 403},
	} {
		r := httptest.NewRequest("GET", "http://customer.example:3001/agent/models?check=1", nil)
		if tc.password != "" {
			r.SetBasicAuth("admin", tc.password)
		}
		r.Header.Set("Origin", tc.origin)
		r.Header.Set("Sec-Fetch-Site", tc.site)
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, r)
		if w.Code != tc.code {
			t.Fatalf("status %d, want %d", w.Code, tc.code)
		}
		if tc.code == 200 && (w.Body.String() != "data: ready\n\n" || w.Header().Get("Set-Cookie") == "" || r.Host != target.Host) {
			t.Fatal("stream, session cookie or local Host was lost")
		}
	}
	if calls != 1 {
		t.Fatalf("unauthorized requests reached the Agent: %d", calls)
	}
}

func TestAdminGatewayFlushesStreamingResponses(t *testing.T) {
	release := make(chan struct{})
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, "data: ready\n\n")
		w.(http.Flusher).Flush()
		<-release
	}))
	defer upstream.Close()
	target, _ := url.Parse(upstream.URL)
	gateway := httptest.NewServer(adminGateway(target, "customer-test-password"))
	defer gateway.Close()
	// Unblock the upstream before closing either HTTP server, including on failure.
	defer close(release)
	client := &http.Client{Timeout: 3 * time.Second}
	r, _ := http.NewRequest("GET", gateway.URL+"/agent/tasks/test/stream", nil)
	r.SetBasicAuth("admin", "customer-test-password")
	response, err := client.Do(r)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	buf := make([]byte, len("data: ready\n\n"))
	if _, err := io.ReadFull(response.Body, buf); err != nil {
		t.Fatalf("event was buffered until stream close: %v", err)
	}
	if response.StatusCode != http.StatusOK || string(buf) != "data: ready\n\n" {
		t.Fatal("stream event changed in transit")
	}
}

func TestAdminGatewayProtectsEveryRouteAndRedirectsRoot(t *testing.T) {
	target, _ := url.Parse("http://127.0.0.1:1")
	handler := adminGateway(target, "customer-test-password")
	for _, path := range []string{"/", "/agent/", "/agent/settings", "/agent/settings/data", "/agent/assets/app.js", "/agent/tasks/events"} {
		for _, method := range []string{"GET", "PUT", "POST"} {
			r := httptest.NewRequest(method, "http://customer.example"+path, nil)
			w := httptest.NewRecorder()
			handler.ServeHTTP(w, r)
			if w.Code != http.StatusUnauthorized || w.Header().Get("WWW-Authenticate") == "" {
				t.Fatalf("%s %s bypassed administrator authentication", method, path)
			}
		}
	}
	r := httptest.NewRequest("GET", "http://customer.example/", nil)
	r.SetBasicAuth("admin", "customer-test-password")
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, r)
	if w.Code != http.StatusTemporaryRedirect || w.Header().Get("Location") != "/agent/" {
		t.Fatal("authenticated root did not redirect to Agent UI")
	}
}
