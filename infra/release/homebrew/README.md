# Homebrew publication status

`ffdb-host.rb.in` is release-time scaffolding for the architecture-neutral host
controller. `scripts/build-release-bundle.sh` renders a checksum-pinned formula
alongside each release, and CI syntax-checks it when Homebrew is available.

There is no public FFDB tap today, so documentation must not advertise `brew
install`. Publishing the generated formula requires all of the following:

1. the versioned `ffdb-host` asset is publicly reachable at the rendered URL;
2. the FFDB release checksum/signature workflow has completed;
3. a maintained tap repository reviews and commits the generated formula;
4. Docker Engine/Desktop with Compose remains an explicit runtime prerequisite.

The supported server artifact is the signed multi-architecture container
bundle. Native `.deb`, `.rpm`, and macOS service packages are outside the
current distribution contract because they would duplicate the container
bundle's dependency, update, and hardening lifecycle.
