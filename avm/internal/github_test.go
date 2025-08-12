package internal

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestGetLatestVersion(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/latest" {
			t.Fatalf("unexpected path: %s", r.URL.Path)
		}
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{"tag_name": "v1.2.3"}`))
	}))
	defer server.Close()

	originalURL := GithubAPIURL
	GithubAPIURL = server.URL
	defer func() { GithubAPIURL = originalURL }()

	version, err := GetLatestVersion()
	if err != nil {
		t.Fatalf("GetLatestVersion returned an error: %v", err)
	}

	if version != "v1.2.3" {
		t.Errorf("expected version v1.2.3, got %s", version)
	}
}

func TestGetRelease(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		expectedPath := fmt.Sprintf("/tags/%s", "v1.2.3")
		if r.URL.Path != expectedPath {
			t.Fatalf("unexpected path: %s, expected: %s", r.URL.Path, expectedPath)
		}
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{"tag_name": "v1.2.3", "assets": [{"name": "argocd-linux-amd64", "browser_download_url": "http://example.com/argocd"}]}`))
	}))
	defer server.Close()

	originalURL := GithubAPIURL
	GithubAPIURL = server.URL
	defer func() { GithubAPIURL = originalURL }()

	release, err := GetRelease("v1.2.3")
	if err != nil {
		t.Fatalf("GetRelease returned an error: %v", err)
	}

	if release.TagName != "v1.2.3" {
		t.Errorf("expected tag_name v1.2.3, got %s", release.TagName)
	}

	if len(release.Assets) != 1 {
		t.Fatalf("expected 1 asset, got %d", len(release.Assets))
	}

	if release.Assets[0].Name != "argocd-linux-amd64" {
		t.Errorf("expected asset name argocd-linux-amd64, got %s", release.Assets[0].Name)
	}
}

func TestGetAllReleases(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" { // The base URL of the server is the releases endpoint
			t.Fatalf("unexpected path: %s", r.URL.Path)
		}
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`[{"tag_name": "v1.2.3"}, {"tag_name": "v1.2.4"}]`))
	}))
	defer server.Close()

	originalURL := GithubAPIURL
	GithubAPIURL = server.URL
	defer func() { GithubAPIURL = originalURL }()

	releases, err := GetAllReleases()
	if err != nil {
		t.Fatalf("GetAllReleases returned an error: %v", err)
	}

	if len(releases) != 2 {
		t.Fatalf("expected 2 releases, got %d", len(releases))
	}

	if releases[0].TagName != "v1.2.3" {
		t.Errorf("expected first release tag_name v1.2.3, got %s", releases[0].TagName)
	}

	if releases[1].TagName != "v1.2.4" {
		t.Errorf("expected second release tag_name v1.2.4, got %s", releases[1].TagName)
	}
}
