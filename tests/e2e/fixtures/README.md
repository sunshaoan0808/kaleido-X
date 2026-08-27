# fixtures

E2E tests inject a localStorage snapshot to simulate a logged-in user. The
snapshot is a **real user session** (contains tokens / session ids) and is
**never committed**. Generate it locally by dumping localStorage from a running
dev browser (CDP 9222), then force
`{"mode":"day","syncServer":false}` for appearance to avoid server-side theme
override.

Without a snapshot, provide `KALEIDO_TEST_ADMIN_USER` / `KALEIDO_TEST_ADMIN_PASSWORD`
to mint a fresh token per test.