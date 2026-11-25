# Changelog

## 0.7.3

Plan requests that sent an unreadable `as` value now receive an error naming the argument instead of silently getting the default result or report format. Omit `as` to keep the default. Intray drops distinguish missing `content` from non-inline `content`. No persisted state changes.
