package session

import "testing"

func TestAddress(t *testing.T) {
	s := Session{Source: "local", Name: "editor"}
	if got := s.Address(); got != "local/editor" {
		t.Fatalf("Address() = %q, want %q", got, "local/editor")
	}
}

func TestParseTarget(t *testing.T) {
	tests := []struct {
		in         string
		wantSource string
		wantName   string
		wantErr    bool
	}{
		{"local/editor", "local", "editor", false},
		{"prod/api", "prod", "api", false},
		{"host/a/b", "host", "a/b", false}, // session names may contain "/"
		{"noslash", "", "", true},
		{"", "", "", true},
		{"/leading", "", "", true},  // empty source
		{"trailing/", "", "", true}, // empty name
	}
	for _, tt := range tests {
		got, err := ParseTarget(tt.in)
		if tt.wantErr {
			if err == nil {
				t.Errorf("ParseTarget(%q) = %+v, want error", tt.in, got)
			}
			continue
		}
		if err != nil {
			t.Errorf("ParseTarget(%q) unexpected error: %v", tt.in, err)
			continue
		}
		if got.Source != tt.wantSource || got.Name != tt.wantName {
			t.Errorf("ParseTarget(%q) = {%q,%q}, want {%q,%q}", tt.in, got.Source, got.Name, tt.wantSource, tt.wantName)
		}
	}
}

func TestLocalSource(t *testing.T) {
	if LocalSource != "local" {
		t.Fatalf("LocalSource = %q, want %q", LocalSource, "local")
	}
}
