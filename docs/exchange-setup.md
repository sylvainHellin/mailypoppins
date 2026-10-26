# Microsoft 365 and Exchange setup

A Microsoft 365 (Exchange Online) account goes through Microsoft Graph (`auth_method = "graph"`).
An on-premises Exchange server is a password account over IMAP and SMTP, like any other IMAP server.

## Microsoft 365: use the mailypoppins app

A Graph account (`mp config init` or `mp config add-account`, option 3, or the desktop app's "Microsoft 365 (Graph API)" preset) needs no app registration of its own.
mailypoppins has one shared multi-tenant registration (any Entra tenant plus personal Microsoft accounts) in its own `mailypoppins.onmicrosoft.com` tenant:

- client ID `439d7ebd-7898-4df2-b2d6-88bb61e2597b`, built in as `MAILYPOPPINS_CLIENT_ID` in `crates/mp-core/src/oauth2.rs`;
- delegated Graph permissions `User.Read`, `Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `offline_access`, `Calendars.ReadWrite`.

The wizard asks for a client ID only for an organisation's own app registration:

```
OAuth2 Configuration
  Press Enter at both prompts to sign in with any work, school or personal Microsoft account.
Client ID [press Enter for the mailypoppins app]:
Tenant ID or domain [common]:
```

Enter at the client ID prompt writes the mailypoppins app's client ID to the account's `[accounts.oauth2]` section.
Enter at the tenant prompt writes `common`, which takes work or school and personal Microsoft accounts alike; a tenant ID or domain limits the sign-in to that organisation.
The device-code, token and refresh endpoints all take the tenant as it is written, so `common` and an explicit tenant both keep working after the first sign-in.
An existing account with its own client ID and tenant ID is read unchanged.

Until the publisher is verified (PERSO-163), only an organisation's admin can consent, by ticking "Consent on behalf of your organization" at the first device-code sign-in.

The account block the wizard writes:

```toml
[[accounts]]
name = "work"
default_from = "user@example.com"
auth_method = "graph"

[accounts.oauth2]
client_id = "439d7ebd-7898-4df2-b2d6-88bb61e2597b"
tenant_id = "common"
```

A Graph account has no `[accounts.smtp]` or `[accounts.imap]` table.
After writing one by hand, sign it in with:

```
mp config oauth2-login --account work
```

The CLI prints a URL and a code; open the URL, enter the code and sign in with the Microsoft account.
The token is cached encrypted at `<mailypoppins_data_dir>/tokens/<account>.enc` (`~/Library/Application Support/mailypoppins/tokens/` on macOS).
It is refreshed when it expires: access tokens last about an hour, refresh tokens about 90 days.

## Microsoft 365: an organisation's own app registration

An organisation that does not want to consent to the shared app registers its own:

1. In the Azure portal (https://portal.azure.com), open "Microsoft Entra ID", then "App registrations", then "New registration".
2. Name it `mailypoppins` (only a label in your tenant), choose "Accounts in this organizational directory only", leave the redirect URI blank, and register.
3. Under "Authentication", "Advanced settings", set "Allow public client flows" to **Yes** and save: the device code flow needs it.
4. Under "API permissions", add the delegated Microsoft Graph permissions `User.Read`, `Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `offline_access` and `Calendars.ReadWrite`; grant admin consent if the portal says "Needs admin approval".
5. Copy the "Application (client) ID" and the "Directory (tenant) ID" from the "Overview" page and type them at the wizard's two prompts, or into `[accounts.oauth2]`.

## On-premises Exchange

An Exchange server the organisation runs itself is reached over IMAP and SMTP with a password.
Choose option 1, "Standard IMAP/SMTP with a password", in `mp config init`, or "IMAP and SMTP" in the desktop wizard, and enter the server names and ports the organisation publishes.
The password is stored with `mp config set-password smtp --account <name>` (and `imap` when it differs).

## Accounts that still say `auth_method = "oauth2"`

The mode that signed in to Microsoft and then spoke IMAP and SMTP with XOAUTH2 is retired.
A config that still names it loads, and that account stays blocked while every other account runs; `mp sync`, `mp config show`, the daemon log and the desktop Settings say how to switch:

- for Microsoft 365, set `auth_method = "graph"`, remove the `[accounts.smtp]` and `[accounts.imap]` tables, keep `[accounts.oauth2]`, and run `mp config oauth2-login --account <name>`;
- for on-premises Exchange, remove the `auth_method` line and `[accounts.oauth2]`, keep the server tables, and store the password with `mp config set-password smtp --account <name>`.

Then restart the daemon with `mp daemon restart`, or press Reload in the desktop Settings.

## Troubleshooting

### "Needs admin consent"
The tenant requires an administrator to consent to the app.
Ask the IT administrator to consent at the first sign-in, or to grant consent in the Azure portal (Entra ID > App registrations > [your app] > API permissions > Grant admin consent).

### Token expired
If a Graph account reports an expired or missing token, run:
```
mp config oauth2-login --account work
```
Refresh tokens expire after about 90 days of inactivity.
