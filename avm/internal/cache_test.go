package internal

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestGetRemoteVersionsWithCache(t *testing.T) {
	// Setup temporary home directory
	tempHome, err := os.MkdirTemp("", "avm-test-home-*")
	if err != nil {
		t.Fatalf("failed to create temp home: %v", err)
	}
	defer os.RemoveAll(tempHome)

	// Mock os.UserHomeDir
	originalHome := os.Getenv("HOME")
	os.Setenv("HOME", tempHome)
	defer os.Setenv("HOME", originalHome)

	cacheDir := filepath.Join(tempHome, ".avm", "cache")
	cacheFile := filepath.Join(cacheDir, "versions.json")

	// 1. Test cache miss with cacheOnly=true
	versions, err := GetRemoteVersionsWithCache(true, false)
	if err != nil {
		t.Errorf("unexpected error on cache miss (cacheOnly=true): %v", err)
	}
	if versions != nil {
		t.Errorf("expected nil versions on cache miss (cacheOnly=true), got %v", versions)
	}

	// 2. Test cache miss with cacheOnly=false (should fetch and save)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`[{"tag_name": "v1.0.0"}]`))
	}))
	defer server.Close()

	originalURL := GithubAPIURL
	GithubAPIURL = server.URL
	defer func() { GithubAPIURL = originalURL }()

	versions, err = GetRemoteVersionsWithCache(false, false)
	if err != nil {
		t.Fatalf("unexpected error on cache fetch: %v", err)
	}
	if len(versions) != 1 || versions[0] != "v1.0.0" {
		t.Errorf("expected [v1.0.0], got %v", versions)
	}

	// Verify cache file exists
	if _, err := os.Stat(cacheFile); os.IsNotExist(err) {
		t.Errorf("cache file was not created")
	}

	// 3. Test cache hit with cacheOnly=true
	// Stop the server to ensure it doesn't hit the network
	server.Close()
	GithubAPIURL = "http://localhost:0" // Invalid URL

	versions, err = GetRemoteVersionsWithCache(true, false)
	if err != nil {
		t.Errorf("unexpected error on cache hit: %v", err)
	}
	if len(versions) != 1 || versions[0] != "v1.0.0" {
		t.Errorf("expected [v1.0.0] from cache, got %v", versions)
	}

	// 4. Test expired cache hit with cacheOnly=true (should still return stale)
	// Manually update cache timestamp to be old
	data, _ := os.ReadFile(cacheFile)
	var cache VersionCache
	json.Unmarshal(data, &cache)
	cache.UpdatedAt = time.Now().Add(-48 * time.Hour)
	data, _ = json.Marshal(cache)
	os.WriteFile(cacheFile, data, 0644)

	versions, err = GetRemoteVersionsWithCache(true, false)
	if err != nil {
		t.Errorf("unexpected error on expired cache hit (cacheOnly=true): %v", err)
	}
	if len(versions) != 1 || versions[0] != "v1.0.0" {
		t.Errorf("expected [v1.0.0] from stale cache, got %v", versions)
	}

	// 5. Test expired cache hit with cacheOnly=false (should try to refresh)
	// We expect it to fail refresh and return stale
	versions, err = GetRemoteVersionsWithCache(false, false)
	if err != nil {
		t.Errorf("unexpected error on expired cache refresh failure: %v", err)
	}
	if len(versions) != 1 || versions[0] != "v1.0.0" {
		t.Errorf("expected [v1.0.0] from stale cache after refresh failure, got %v", versions)
	}

	// 6. Test force refresh
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`[{"tag_name": "v1.1.0"}]`))
	}))
	defer server.Close()
	GithubAPIURL = server.URL

	versions, err = GetRemoteVersionsWithCache(false, true)
	if err != nil {
		t.Fatalf("unexpected error on forced refresh: %v", err)
	}
	if len(versions) != 1 || versions[0] != "v1.1.0" {
		t.Errorf("expected [v1.1.0] after forced refresh, got %v", versions)
	}
}
