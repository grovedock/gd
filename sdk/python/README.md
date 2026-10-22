# grovedock (Python)

Python client for [Grovedock](https://github.com/grovedock/gd): read files, list
directories, search, and inspect history at any revision without cloning.

```python
from grovedock import Client

repo = Client("http://127.0.0.1:7420", token=None)
print(repo.read_file("src/lib.rs").decode())
for cs in repo.log(path="src", limit=10):
    print(cs.id[:12], cs.author, cs.summary)
print(repo.diff("main~1-id", "main"))  # changeset ids or ref names
```

Changeset contents are verified against their BLAKE3 ids (except changesets the server
redacted because of your read permissions, which are marked `redacted=True`).
