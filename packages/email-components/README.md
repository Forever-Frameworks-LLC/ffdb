# `@ffdb/email-components`

Version-matched React Email defaults for FFDB verification, password reset,
email-change, invitation, and magic-link messages.

```bash
pnpm add ./ffdb-email-components-0.3.0.tgz @react-email/components react
```

`ffdb-email-components-0.3.0.tgz` is the checksum-verified archive from the
matching FFDB GitHub Release. The archive retains the
`@ffdb/email-components` package name, so imports remain scoped.

```tsx
import { PasswordResetEmail, templateManifest } from "@ffdb/email-components";

const message = (
  <PasswordResetEmail
    projectName="Atlas"
    actionUrl="https://app.example.com/reset/one-time-token"
    expiresIn="30 minutes"
  />
);
```

`templateManifest` is the source of truth for each template kind's allowed
runtime variables and default subject. Components produce React Email markup;
FFDB's isolated compiler renders and validates the artifact before the Rust
delivery worker can use it. Do not put provider credentials or untrusted raw
HTML in component props.

This package does not send email and does not include an email-provider API
key. Use the version that matches the FFDB server release.
