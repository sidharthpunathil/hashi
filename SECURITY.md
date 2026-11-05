# Security Policy

Hashi lets HTTP clients run coding agents on the host machine, so security reports get priority.

## Reporting a vulnerability

Please do not open a public issue. Report privately through GitHub's [private vulnerability reporting](https://github.com/sidharthpunathil/hashi/security/advisories/new).

Include the version (`hashi --version`), what you did, what happened, and what you expected. You will get an acknowledgement within a few days. A fix is coordinated with you before anything is disclosed.

## In scope

- Authentication or authorisation bypass (for example, using a revoked or expired token, or reading another token's requests)
- Ways to make Hashi pass attacker-controlled flags to an agent
- Escaping the per-token workspace or the configured agent sandbox through Hashi itself
- Secrets leaking into logs, API responses or the database in clear text
- Denial of service that gets past the documented limits

## Out of scope

- Anything a valid admin token is designed to allow
- Behaviour of Claude Code, Codex or cloudflared themselves (report those upstream)
- Exposing Hashi on a public interface without a tunnel or reverse proxy, after the warning it prints

## Supported versions

Security fixes go into the latest release.
