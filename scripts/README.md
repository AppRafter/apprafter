# scripts/

Repository-level utility scripts. Each is a standalone executable
(`chmod +x`) and is intended to be callable directly during local
development. Future phase 0.6 will wire these into a Justfile;
phase 0.5 will invoke them from CI.

| Script         | Purpose                                       |
| -------------- | --------------------------------------------- |
| `lint-cue.sh`  | Format check and vet of all CUE files.        |
