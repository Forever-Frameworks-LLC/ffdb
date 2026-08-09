# FFDB documentation refresh brief

## Product and audience

FFDB is an Apache-2.0 backend that keeps application data in an isolated SQLite database per project while PostgreSQL coordinates the control plane. The documentation must serve two primary readers:

1. An operator deciding how to install, secure, upgrade, back up, and observe a self-hosted deployment.
2. An application developer adding authentication, queries, storage, and offline synchronization in browser, React, React Native, or Node code.

A future reader may evaluate a paid managed service, but that service is not currently available and must never be presented as purchasable.

## Problem to solve

The landing page promises self-hosting and offline-capable client libraries, while the current docs make the repository quickstart easier to find than Docker/systemd installation and leave several feature pages too conceptual. Code blocks also lack useful syntax treatment. Public examples must be verifiable against the current CLI and TypeScript packages, including the fact that the `@ffdb/*` packages are not yet published to npm.

## Success criteria

- Docker and systemd are first-class installation paths from the docs introduction, navigation, and quickstart.
- Every product page answers what the capability does, when to use it, how to configure/use it, and how to verify or troubleshoot it.
- `OfflineSyncClient` is described as a runtime-neutral orchestration engine and each runtime's required persistence adapter is explicit.
- Code and CLI samples match current exported types, commands, routes, and distribution status.
- Code blocks have accessible syntax highlighting, a language label, and a working copy action.
- Landing copy speaks to outcomes for the application builder/operator while retaining the owned visual system.
- Managed FFDB is labeled as a possible future paid option, not an active product or pricing promise.

## Constraints

- Preserve existing landing/docs design systems and routing model.
- Do not invent published images, packages, install scripts, billing, SLAs, or managed-service capabilities.
- Keep navigation at two levels and keep operational cautions close to the commands they govern.
