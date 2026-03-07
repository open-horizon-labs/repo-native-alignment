---
id: repo-native
severity: hard
---

# Repo-Native Storage

No external store, no platform dependency for core function. All alignment artifacts live in the repo as files. If you `rm -rf .oh/`, you lose context but nothing breaks. Git provides versioning. OH integration is optional sync, not a requirement.
