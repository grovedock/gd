package gd

import (
	"context"
	"os"
	"reflect"
	"testing"
)

// Runs against a seeded server: eval "$(../test-server.sh)" (sets GD_URL).
func client(t *testing.T) *Client {
	url := os.Getenv("GD_URL")
	if url == "" {
		t.Skip("GD_URL not set (start ../test-server.sh)")
	}
	c, err := New(url, "")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	return c
}

func TestReadsWithoutAClone(t *testing.T) {
	c, ctx := client(t), context.Background()
	got, err := c.ReadFile(ctx, "main", "src/lib.rs")
	if err != nil || string(got) != "pub fn answer() -> u32 { 42 }\n" {
		t.Fatalf("ReadFile = %q, %v", got, err)
	}
	entries, err := c.ListDir(ctx, "main", "")
	if err != nil || len(entries) != 3 || entries[1].Name != "docs" || entries[1].Mode != "dir" {
		t.Fatalf("ListDir = %+v, %v", entries, err)
	}
	if found, _ := c.Find(ctx, "main", "guide", 10); !reflect.DeepEqual(found, []string{"docs/guide.md"}) {
		t.Fatalf("Find = %v", found)
	}
}

func TestHistoryDiffBlame(t *testing.T) {
	c, ctx := client(t), context.Background()
	log, err := c.Log(ctx, "main", "", 10)
	if err != nil || len(log) != 2 || log[0].Message != "fix the answer" || log[0].Parents[0] != log[1].ID {
		t.Fatalf("Log = %+v, %v", log, err)
	}
	diff, err := c.Diff(ctx, log[1].ID, log[0].ID)
	if err != nil || len(diff) != 2 {
		t.Fatalf("Diff = %+v, %v", diff, err)
	}
	blame, err := c.Blame(ctx, "main", "src/lib.rs")
	if err != nil || len(blame) != 1 || blame[0].Changeset != log[0].ID {
		t.Fatalf("Blame = %+v, %v", blame, err)
	}
	refs, _ := c.Refs(ctx)
	if refs["main"] != log[0].ID {
		t.Fatalf("Refs = %v", refs)
	}
	if mb, _ := c.MergeBase(ctx, log[1].ID, log[0].ID); mb != log[1].ID {
		t.Fatalf("MergeBase = %s", mb)
	}
	cs, err := c.Changeset(ctx, log[1].ID)
	if err != nil || cs.Message != "initial" {
		t.Fatalf("Changeset = %+v, %v", cs, err)
	}
}
