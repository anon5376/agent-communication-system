Environment notes for this benchmark task:

- The repository checkout you were started in is at the commit the task is about. Read it; do not change tracked files. Other branches and remotes are fetched read-only: use `git show <ref>:<path>` rather than checking them out.
- Submit the answer as JSON in the `details` of `qagent task submit` (the `--details` flag), and a one-line `--summary`. The JSON shape is:

  {
    "items": ["short identifier of each answer item, as the task names the form"],
    "claims": [{ "text": "one factual statement", "evidence": ["src/example.ts:120", "https://primary-source-url"] }],
    "unresolved": ["each question you could not settle, with what is missing"]
  }

- `items` is the answer set the question asks for. Every claim needs evidence: a `path:line` in the repository at the task's commit, a command and its output, or a primary-source URL. A claim without evidence counts against you.
- Leave unresolved what you could not settle. Do not guess to fill a list.
