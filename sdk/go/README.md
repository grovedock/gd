# Grovedock Go client

```go
import gd "github.com/grovedock/gd/sdk/go/v2"

c, _ := gd.New("http://127.0.0.1:7420", "")
defer c.Close()
data, _ := c.ReadFile(ctx, "main", "src/lib.rs")
log, _ := c.Log(ctx, "main", "src", 10)
```

Changesets and file contents are verified against their BLAKE3 ids (`ErrIntegrity`).
