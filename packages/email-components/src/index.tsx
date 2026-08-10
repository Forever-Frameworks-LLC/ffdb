import {
  Body,
  Button,
  Container,
  Head,
  Heading,
  Hr,
  Html,
  Preview,
  Section,
  Text,
} from "@react-email/components";

export type EmailTemplateKind =
  | "verification"
  | "password_reset"
  | "email_change"
  | "invitation"
  | "magic_link";

export interface TransactionalEmailProps {
  readonly projectName: string;
  readonly actionUrl: string;
  readonly expiresIn: string;
  readonly recipientEmail?: string;
}

interface Copy {
  readonly preview: string;
  readonly heading: string;
  readonly explanation: string;
  readonly action: string;
}

const copy: Readonly<Record<EmailTemplateKind, Copy>> = {
  verification: {
    preview: "Verify your email",
    heading: "Verify your email",
    explanation: "Confirm this address to finish creating your account.",
    action: "Verify email",
  },
  password_reset: {
    preview: "Reset your password",
    heading: "Reset your password",
    explanation: "Use this secure link to choose a new password.",
    action: "Reset password",
  },
  email_change: {
    preview: "Confirm your new email",
    heading: "Confirm your new email",
    explanation: "Confirm this address to complete your email change.",
    action: "Confirm email",
  },
  invitation: {
    preview: "You were invited to collaborate",
    heading: "Join the project",
    explanation: "You have been invited to collaborate on this project.",
    action: "Accept invitation",
  },
  magic_link: {
    preview: "Your secure sign-in link",
    heading: "Sign in securely",
    explanation: "Use this one-time link to continue.",
    action: "Sign in",
  },
};

export function TransactionalEmail({
  kind,
  projectName,
  actionUrl,
  expiresIn,
  recipientEmail,
}: TransactionalEmailProps & { readonly kind: EmailTemplateKind }) {
  const content = copy[kind];
  return (
    <Html lang="en">
      <Head />
      <Preview>{`${content.preview} · ${projectName}`}</Preview>
      <Body style={styles.body}>
        <Container style={styles.container}>
          <Text style={styles.brand}>FFDB / {projectName}</Text>
          <Heading style={styles.heading}>{content.heading}</Heading>
          <Text style={styles.paragraph}>{content.explanation}</Text>
          <Section style={styles.actionSection}>
            <Button href={actionUrl} style={styles.button}>
              {content.action}
            </Button>
          </Section>
          <Text style={styles.paragraph}>This link expires in {expiresIn}.</Text>
          {recipientEmail === undefined ? null : (
            <Text style={styles.muted}>This message was sent to {recipientEmail}.</Text>
          )}
          <Hr style={styles.rule} />
          <Text style={styles.footer}>If you did not request this action, you can safely ignore this email.</Text>
        </Container>
      </Body>
    </Html>
  );
}

export const VerificationEmail = (props: TransactionalEmailProps) => (
  <TransactionalEmail kind="verification" {...props} />
);
export const PasswordResetEmail = (props: TransactionalEmailProps) => (
  <TransactionalEmail kind="password_reset" {...props} />
);
export const EmailChangeEmail = (props: TransactionalEmailProps) => (
  <TransactionalEmail kind="email_change" {...props} />
);
export const InvitationEmail = (props: TransactionalEmailProps) => (
  <TransactionalEmail kind="invitation" {...props} />
);
export const MagicLinkEmail = (props: TransactionalEmailProps) => (
  <TransactionalEmail kind="magic_link" {...props} />
);

export interface TemplateManifestEntry {
  readonly kind: EmailTemplateKind;
  readonly allowedVariables: readonly string[];
  readonly defaultSubject: string;
}

export const templateManifest: readonly TemplateManifestEntry[] = [
  { kind: "verification", allowedVariables: ["project_name", "action_url", "expires_in"], defaultSubject: "Verify your email for {{project_name}}" },
  { kind: "password_reset", allowedVariables: ["project_name", "action_url", "expires_in"], defaultSubject: "Reset your password for {{project_name}}" },
  { kind: "email_change", allowedVariables: ["project_name", "action_url", "expires_in"], defaultSubject: "Confirm your new {{project_name}} email" },
  { kind: "invitation", allowedVariables: ["project_name", "action_url", "expires_in"], defaultSubject: "You were invited to {{project_name}}" },
  { kind: "magic_link", allowedVariables: ["project_name", "action_url", "expires_in"], defaultSubject: "Sign in to {{project_name}}" },
] as const;

const styles = {
  body: {
    backgroundColor: "#f7f8fa",
    color: "#101828",
    fontFamily: "Inter, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
    margin: "0",
    padding: "40px 12px",
  },
  container: {
    backgroundColor: "#ffffff",
    border: "1px solid #d6dce5",
    borderRadius: "6px",
    margin: "0 auto",
    maxWidth: "560px",
    padding: "32px",
  },
  brand: { color: "#0868e8", fontSize: "13px", fontWeight: "700", margin: "0 0 28px" },
  heading: { color: "#101828", fontSize: "28px", lineHeight: "34px", margin: "0 0 16px" },
  paragraph: { color: "#344054", fontSize: "15px", lineHeight: "24px", margin: "0 0 18px" },
  muted: { color: "#667085", fontSize: "13px", lineHeight: "20px" },
  actionSection: { margin: "26px 0" },
  button: { backgroundColor: "#0868e8", borderRadius: "5px", color: "#ffffff", fontSize: "14px", fontWeight: "600", padding: "12px 18px", textDecoration: "none" },
  rule: { borderColor: "#d6dce5", margin: "28px 0 18px" },
  footer: { color: "#667085", fontSize: "12px", lineHeight: "18px", margin: "0" },
} as const;
