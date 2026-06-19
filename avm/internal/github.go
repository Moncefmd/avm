package internal

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"
)

var (
	// GithubAPIURL is the base URL for the GitHub API. It can be changed for testing.
	GithubAPIURL = "https://api.github.com/repos/argoproj/argo-cd/releases"
)

type VersionCache struct {
	Versions  []string  `json:"versions"`
	UpdatedAt time.Time `json:"updated_at"`
}

type Release struct {
	TagName string  `json:"tag_name"`
	Assets  []Asset `json:"assets"`
}

type Asset struct {
	Name               string `json:"name"`
	BrowserDownloadURL string `json:"browser_download_url"`
}

func GetLatestVersion() (string, error) {
	resp, err := http.Get(GithubAPIURL + "/latest")
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()

	var release Release
	if err := json.NewDecoder(resp.Body).Decode(&release); err != nil {
		return "", err
	}

	return release.TagName, nil
}

func GetRelease(version string) (*Release, error) {
	url := fmt.Sprintf("%s/tags/%s", GithubAPIURL, version)
	resp, err := http.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("failed to get release %s: %s", version, resp.Status)
	}

	var release Release
	if err := json.NewDecoder(resp.Body).Decode(&release); err != nil {
		return nil, err
	}

	return &release, nil
}

func GetAllReleases() ([]Release, error) {
	resp, err := http.Get(GithubAPIURL)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("failed to get releases: %s", resp.Status)
	}

	var releases []Release
	if err := json.NewDecoder(resp.Body).Decode(&releases); err != nil {
		return nil, err
	}

	return releases, nil
}

func GetRemoteVersions() ([]string, error) {
	// For manual listing, we always force a refresh to update the cache
	return GetRemoteVersionsWithCache(false, true)
}

func GetRemoteVersionsWithCache(cacheOnly bool, forceRefresh bool) ([]string, error) {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}

	cacheDir := filepath.Join(homeDir, ".avm", "cache")
	cacheFile := filepath.Join(cacheDir, "versions.json")

	cachedVersions, updatedAt, loadErr := loadCache(cacheFile)

	if cacheOnly {
		if loadErr == nil {
			return cachedVersions, nil
		}
		// If cache is missing or corrupted and we're in cacheOnly mode, return nil
		// to avoid network latency during autocompletion.
		return nil, nil
	}

	// If not forcing refresh, check if cache is still valid (24h)
	if !forceRefresh && loadErr == nil && time.Since(updatedAt) < 24*time.Hour {
		return cachedVersions, nil
	}

	// Fetch from remote
	releases, err := GetAllReleases()
	if err != nil {
		// If remote fetch fails, but we have a cache, use it even if stale
		if cachedVersions != nil {
			return cachedVersions, nil
		}
		return nil, err
	}

	versions := make([]string, len(releases))
	for i, release := range releases {
		versions[i] = release.TagName
	}

	// Save to cache
	_ = os.MkdirAll(cacheDir, 0755)
	cacheData := VersionCache{
		Versions:  versions,
		UpdatedAt: time.Now(),
	}
	data, err := json.Marshal(cacheData)
	if err == nil {
		_ = os.WriteFile(cacheFile, data, 0644)
	}

	return versions, nil
}

func loadCache(cacheFile string) ([]string, time.Time, error) {
	data, err := os.ReadFile(cacheFile)
	if err != nil {
		return nil, time.Time{}, err
	}

	var cache VersionCache
	if err := json.Unmarshal(data, &cache); err != nil {
		return nil, time.Time{}, err
	}

	return cache.Versions, cache.UpdatedAt, nil
}
