# Security policy

Please report vulnerabilities privately through GitHub's **Report a vulnerability** button
(Security tab), not in a public issue.

Locus stores connection passwords in the Windows Credential Manager and writes an audit log
for Production-protected connections. Reports about credential handling, the SSH tunnel, the
Production read-only guard, or the Go sidecar protocol are especially welcome.

Release downloads are signed with minisign; the public key is [`minisign.pub`](minisign.pub)
(`RWRDNREpYa8EZ59rUGsAnTVNIMjNfUWhciyq6SgS2MpNZvMGj0yVAZ0s`). A signature from any other key did
not come from this project's release workflow.

Only the latest release receives fixes.
