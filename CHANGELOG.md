# Changelog

## Timeout Resilience

- **Retried Transient Requests**: Every WebDAV request (PROPFIND, MKCOL, PUT, GET, DELETE) now retries up to `REQUEST_MAX_ATTEMPTS` (3) times with a growing delay when it fails with a transient transport error — a timeout, connection reset, or truncated body. Permanent failures (bad credentials, 403, local disk errors) are not retried.
- **Timeouts No Longer Abort the Whole Action**: A request that still fails after every retry skips only its own item. `put`/`sync` continue uploading the remaining files, `get` removes the partial download and moves on, and the recursive scan skips the unlisted subfolder — warning loudly and listing every skipped directory in the final summary — instead of failing the entire operation. Only the root listing remains fatal, since there is nothing to traverse without it.
- **Verbose Retry Diagnostics**: Each retry prints the operation, attempt number, elapsed time, whether the error was a timeout/connect/body failure, and the full reqwest source chain, plus when a request recovers or gives up.

## Concurrent Uploads

- **Parallel PUT/Sync Uploads**: `put` and `sync` now upload files through a worker pool sized by the Settings "Threads" value (the same concurrency setting already used for remote scanning), instead of uploading files one at a time. The pool mirrors the scan design (atomic work cursor, mpsc log funneling, panic recovery) and serializes remote-cache updates behind a mutex because `update_remote_cache_item` performs a read-modify-write of the cache file. Per-file error semantics: MKCOL/open/HTTP-status failures are logged and skipped; a transport failure is retried (see Timeout Resilience above) and skipped once its retries are exhausted.

## WebDAV Security Hardening

- **Symlink Traversal Prevention**: `collect_local_files_with_mtime` skips symlinks and enforces canonical path containment checks under the root source directory to prevent arbitrary file disclosure.
- **Path Traversal Protection**: WebDAV downloads and item path resolutions sanitize and reject relative path sequences (`..`, `.`, leading slashes, backslashes). Download destinations enforce canonical containment under the target directory before writing.
- **SSRF and Protocol Validation**: Added `validate_webdav_url` blocking private RFC 1918 IPs, loopback, link-local / cloud metadata (e.g. 169.254.169.254), and carrier-grade NAT in production, while requiring HTTPS.
- **Streamed I/O & File Size Limits**: Added configurable maximum file size limit (`MAX_WEBDAV_FILE_SIZE_BYTES`, default 500 MB). Uploads and downloads stream without buffering complete file contents into RAM.
- **Safe Remote Parent Directory Creation**: `ensure_remote_parent_dirs` validates target URLs and safely handles `MKCOL` HTTP statuses without ignoring unexpected failures.
- **WebdavItem Builder API**: Added `WebdavItem::new(href, is_dir, size)` and `.with_mtime(...)` builder methods and `Default` implementation for backwards compatibility.
- **File Time Preservation Diagnostics**: Preserving remote modification timestamps on downloaded files now logs diagnostic warnings on failure rather than ignoring errors silently.
- **Deterministic Integration Tests**: E2E tests use `FileTimes` to adjust timestamps explicitly rather than relying on brittle sleep intervals in CI.
- **Local Mount Containment**: `run_webdav_action` strictly enforces `join_contained` for `subdir` under the mount directory and rejects traversal errors instead of falling back to raw path join.
