# grovedock (TypeScript / Node.js)

```ts
import { Client } from "grovedock";

const repo = new Client("http://127.0.0.1:7420");
console.log((await repo.readFile("src/lib.rs")).toString());
for (const cs of await repo.log("main", "src", 10)) console.log(cs.id.slice(0, 12), cs.message);
```

Changesets and file contents are verified against their BLAKE3 ids (`IntegrityError`).
