package internal

import (
	"encoding/json"
	"fmt"
	"net/http"
)

var (
	// GithubAPIURL is the base URL for the GitHub API. It can be changed for testing.
	GithubAPIURL = "https://api.github.com/repos/argoproj/argo-cd/releases"
)

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
