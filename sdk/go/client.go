// Package gd is a Go client for Grovedock (https://github.com/grovedock/gd):
// read files, list directories, search, and inspect history at any revision without
// cloning. Changeset and file contents are verified against their BLAKE3 ids.
package gd

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/url"

	"github.com/grovedock/gd/sdk/go/v2/objectspb"
	pb "github.com/grovedock/gd/sdk/go/v2/gdpb"
	"github.com/zeebo/blake3"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/protobuf/proto"
)

const maxMessage = 260 << 20

// ErrIntegrity reports data that does not match its content id.
var ErrIntegrity = errors.New("gd: content does not match its id")

var modes = map[int32]string{0: "file", 1: "exec", 2: "symlink", 3: "dir"}

// FileChange is a change relative to a changeset's first parent. Blob is empty for deletions.
type FileChange struct {
	Path string
	Blob string
	Mode string
}

// Changeset is a decoded changeset. Redacted ones had changes outside the caller's read
// permissions removed by the server and cannot be verified against their id.
type Changeset struct {
	ID          string
	Parents     []string
	Author      string
	Message     string
	TimestampMs int64
	Changes     []FileChange
	Redacted    bool
}

// DirEntry is a directory listing entry. ID is empty for directories the caller may
// traverse but not read.
type DirEntry struct {
	Name string
	Mode string
	ID   string
}

// Client talks to one Grovedock server.
type Client struct {
	conn  *grpc.ClientConn
	token string
}

// New connects to url ("http://host:7420" or "https://..."); token may be empty.
func New(rawURL, token string) (*Client, error) {
	u, err := url.Parse(rawURL)
	if err != nil {
		return nil, err
	}
	creds := insecure.NewCredentials()
	if u.Scheme == "https" {
		creds = credentials.NewTLS(&tls.Config{MinVersion: tls.VersionTLS12})
	}
	conn, err := grpc.NewClient(u.Host, grpc.WithTransportCredentials(creds),
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(maxMessage), grpc.MaxCallSendMsgSize(maxMessage)))
	if err != nil {
		return nil, err
	}
	return &Client{conn: conn, token: token}, nil
}

// Close releases the connection.
func (c *Client) Close() error { return c.conn.Close() }

func (c *Client) ctx(ctx context.Context) context.Context {
	if c.token == "" {
		return ctx
	}
	return metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+c.token)
}

func idOf(tag byte, data []byte) []byte {
	h := blake3.New()
	h.Write([]byte{tag})
	h.Write(data)
	return h.Sum(nil)
}

func change(c *objectspb.FileChange) FileChange {
	return FileChange{Path: c.Path, Blob: hex.EncodeToString(c.Blob), Mode: modes[int32(c.Mode)]}
}

func decodeChangeset(info *pb.ChangesetInfo) (Changeset, error) {
	if !info.Redacted && !bytes.Equal(idOf(3, info.Encoded), info.Id) {
		return Changeset{}, fmt.Errorf("%w: changeset %x", ErrIntegrity, info.Id)
	}
	var cs objectspb.Changeset
	if err := proto.Unmarshal(info.Encoded, &cs); err != nil {
		return Changeset{}, err
	}
	out := Changeset{ID: hex.EncodeToString(info.Id), Author: cs.Author, Message: cs.Message,
		TimestampMs: cs.TimestampMs, Redacted: info.Redacted}
	for _, p := range cs.Parents {
		out.Parents = append(out.Parents, hex.EncodeToString(p))
	}
	for _, ch := range cs.Changes {
		out.Changes = append(out.Changes, change(ch))
	}
	return out, nil
}

// ReadFile returns the content of path at rev (a ref name or changeset id).
func (c *Client) ReadFile(ctx context.Context, rev, path string) ([]byte, error) {
	r, err := pb.NewBrowseClient(c.conn).ReadFile(c.ctx(ctx), &pb.BrowseRequest{Rev: rev, Path: path})
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(idOf(1, r.Content), r.Id) {
		return nil, fmt.Errorf("%w: %s", ErrIntegrity, path)
	}
	return r.Content, nil
}

// ListDir lists a directory ("" = root) at rev.
func (c *Client) ListDir(ctx context.Context, rev, path string) ([]DirEntry, error) {
	r, err := pb.NewBrowseClient(c.conn).ListDir(c.ctx(ctx), &pb.BrowseRequest{Rev: rev, Path: path})
	if err != nil {
		return nil, err
	}
	out := make([]DirEntry, 0, len(r.Entries))
	for _, e := range r.Entries {
		out = append(out, DirEntry{Name: e.Name, Mode: modes[e.Mode], ID: hex.EncodeToString(e.Id)})
	}
	return out, nil
}

// Find returns paths containing pattern at rev.
func (c *Client) Find(ctx context.Context, rev, pattern string, limit uint32) ([]string, error) {
	r, err := pb.NewBrowseClient(c.conn).Find(c.ctx(ctx), &pb.FindRequest{Rev: rev, Pattern: pattern, Limit: limit})
	if err != nil {
		return nil, err
	}
	return r.Paths, nil
}

// Log returns first-parent history from rev, newest first, optionally limited to path.
func (c *Client) Log(ctx context.Context, rev, path string, limit uint32) ([]Changeset, error) {
	stream, err := pb.NewHistoryClient(c.conn).Log(c.ctx(ctx), &pb.LogRequest{Rev: rev, Path: path, Limit: limit})
	if err != nil {
		return nil, err
	}
	var out []Changeset
	for {
		info, err := stream.Recv()
		if errors.Is(err, io.EOF) {
			return out, nil
		}
		if err != nil {
			return nil, err
		}
		cs, err := decodeChangeset(info)
		if err != nil {
			return nil, err
		}
		out = append(out, cs)
	}
}

// Changeset resolves rev (ref name or id) to a changeset.
func (c *Client) Changeset(ctx context.Context, rev string) (Changeset, error) {
	info, err := pb.NewChangesetsClient(c.conn).Get(c.ctx(ctx), &pb.ResolveRequest{Rev: rev})
	if err != nil {
		return Changeset{}, err
	}
	return decodeChangeset(info)
}

// Diff lists file changes between two revisions.
func (c *Client) Diff(ctx context.Context, from, to string) ([]FileChange, error) {
	r, err := pb.NewDiffClient(c.conn).Diff(c.ctx(ctx), &pb.DiffRequest{From: from, To: to})
	if err != nil {
		return nil, err
	}
	out := make([]FileChange, 0, len(r.Changes))
	for _, ch := range r.Changes {
		out = append(out, FileChange{Path: ch.Path, Blob: hex.EncodeToString(ch.Blob), Mode: modes[ch.Mode]})
	}
	return out, nil
}

// BlameLine attributes one line of a file to the changeset that last changed it.
type BlameLine struct {
	Changeset string
	Text      string
}

// Blame attributes each line of path at rev.
func (c *Client) Blame(ctx context.Context, rev, path string) ([]BlameLine, error) {
	r, err := pb.NewHistoryClient(c.conn).Blame(c.ctx(ctx), &pb.BlameRequest{Rev: rev, Path: path})
	if err != nil {
		return nil, err
	}
	out := make([]BlameLine, 0, len(r.Lines))
	for _, l := range r.Lines {
		out = append(out, BlameLine{Changeset: hex.EncodeToString(l.Changeset), Text: l.Text})
	}
	return out, nil
}

// Refs returns every ref and the changeset it points at.
func (c *Client) Refs(ctx context.Context) (map[string]string, error) {
	r, err := pb.NewRefsClient(c.conn).List(c.ctx(ctx), &pb.Empty{})
	if err != nil {
		return nil, err
	}
	out := make(map[string]string, len(r.Refs))
	for _, e := range r.Refs {
		out[e.Name] = hex.EncodeToString(e.Id)
	}
	return out, nil
}

// MergeBase returns the best common ancestor of a and b ("" if none).
func (c *Client) MergeBase(ctx context.Context, a, b string) (string, error) {
	r, err := pb.NewMergeBaseClient(c.conn).MergeBase(c.ctx(ctx), &pb.MergeBaseRequest{A: a, B: b})
	if err != nil || !r.Found {
		return "", err
	}
	return hex.EncodeToString(r.Id), nil
}
