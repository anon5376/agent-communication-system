Environment notes for this benchmark task:

- Your repository is the git checkout the supervisor started you in; Node dependencies are installed in the benchmark clone's repo root, so in a task worktree run `ln -s <clone-root>/node_modules node_modules` instead of `npm ci`. Do not run plain `npm install`.
- `dist/` is committed and must match the build: run `npm run build` and commit `dist/` together with your source change.
- Commit your work on the branch your worktree has checked out. Work that is not committed is not part of the result.
- Keep the change small and in the area the task names. Add a test when the task asks for one or when the behaviour is new.
- Submit with a summary, the changed files, and the validation you ran (the command and whether it passed).
