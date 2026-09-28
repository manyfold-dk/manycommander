# Deploy manycommander.app for the first time

This runbook connects the site workflow (`.github/workflows/site.yml`) to Cloudflare. The
workflow deploys the Worker `manycommander-site` from `main`. The deploy job runs only when
the repository variable `SITE_DEPLOY` is `true`. Do these steps from a computer that has
access to the Cloudflare account.

The first deploy used this procedure on 2026-09-28. Use the procedure again to rebuild the
setup, for example after the loss of the API token.

## Prerequisites

- Access to the Cloudflare account that holds the zone `manycommander.app`. The zone
  status is `Active`.
- Write access to the configuration that manages the zone as code (OpenTofu), or a
  person who has that access.
- Access to the registrar of `manycommander.app`.
- Admin access to the GitHub repository `manyfold-dk/manycommander`.
- The `gh` command, logged in to GitHub. The GitHub web interface also works.
- The `curl` and `dig` commands.

## Pre-action checklist

- [ ] The last `site` workflow run on `main` is green, and the `deploy` job shows `skipped`.
- [ ] The Cloudflare dashboard shows the zone `manycommander.app` as `Active`.

## Procedure

### Step 1: Prepare the zone

> **Warning:** OpenTofu manages the DNS records and the settings of this zone. OpenTofu
> reverts a change in the Cloudflare dashboard at the next apply. Make each change in the
> OpenTofu configuration, not in the dashboard.

The Worker gets the hostnames `manycommander.app` and `www.manycommander.app` as custom
domains. Cloudflare creates the DNS records for the custom domains. Cloudflare does not
create a custom domain on a hostname that has a `CNAME` record. A redirect rule runs before
the Worker, so a redirect rule on these hostnames hides the site.

1. Remove each `A`, `AAAA` and `CNAME` record on `@` and on `www` from the configuration.
2. Remove each redirect rule that matches `manycommander.app` or `www.manycommander.app`.
3. Set the zone setting **Always Use HTTPS** to on.
4. Add a record: type `MX`, name `@`, mail server `.`, priority `0`.
5. Add a record: type `TXT`, name `@`, content `v=spf1 -all`.
6. Add a record: type `TXT`, name `_dmarc`, content `v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s`.
7. Apply the configuration.

The domain sends no mail and receives no mail. The records of items 4 to 6 tell mail
servers to refuse mail that uses the domain. A domain has one SPF record only.

### Step 2: Enable DNSSEC

1. In the Cloudflare dashboard, open the zone `manycommander.app`.
2. Open **DNS** > **Settings**.
3. Click **Enable DNSSEC**.
4. Copy the DS record values from the dialog.
5. Add the DS record at the registrar of `manycommander.app`.
6. Wait until the Cloudflare dashboard shows DNSSEC as `Active`.

The wait is usually 10 minutes to some hours. If the registrar is Cloudflare Registrar,
Cloudflare adds the DS record, and items 4 and 5 do not apply.

### Step 3: Create the API token

> **Warning:** The token gives write access to the Workers of the account. Do not put the
> token in a file, a chat or a terminal command. Send the token only into the GitHub
> secret. Do not keep a copy: to rotate the token, create a new token.

The deploy needs two permissions only. The template "Edit Cloudflare Workers" gives many
more, so do not use the template.

1. In the Cloudflare dashboard, open **Manage Account** > **Account API Tokens**.
2. Click **Create Token**, then **Create Custom Token**.
3. Name the token `manycommander-site deploy (GitHub Actions)`.
4. Add the permission `Account` > `Workers Scripts` > `Edit`.
5. Add the permission `Zone` > `Workers Routes` > `Edit`.
6. Set **Zone Resources** to `Include`, `Specific zone` and `manycommander.app`.
7. Click **Continue to summary**.
8. Click **Create Token**.
9. Keep the page open for Step 5. Cloudflare shows the token one time only.

### Step 4: Find the account ID

1. In the Cloudflare dashboard, open **Account home**.
2. Open the menu of the account and click **Copy account ID**.

The account ID is not secret, but it identifies the account. Put the account ID only into
the GitHub secret, not into the repository.

### Step 5: Add the GitHub secrets and the variable

1. Run the command below. Paste the API token when `gh` asks for the value.

   ```bash
   gh secret set CLOUDFLARE_API_TOKEN --repo manyfold-dk/manycommander
   ```

2. Run the command below. Paste the account ID when `gh` asks for the value.

   ```bash
   gh secret set CLOUDFLARE_ACCOUNT_ID --repo manyfold-dk/manycommander
   ```

3. Set the variable that enables the deploy job:

   ```bash
   gh variable set SITE_DEPLOY --body true --repo manyfold-dk/manycommander
   ```

In the GitHub web interface, the same values are in **Settings** > **Secrets and
variables** > **Actions**.

### Step 6: Run the deploy

1. Start the workflow on `main`:

   ```bash
   gh workflow run site.yml --repo manyfold-dk/manycommander --ref main
   ```

2. Follow the run:

   ```bash
   gh run watch --repo manyfold-dk/manycommander "$(gh run list --repo manyfold-dk/manycommander --workflow site.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
   ```

3. Confirm that the `build` job and the `deploy` job are green.
4. If the `deploy` job fails with an authentication error on a custom domain, check the
   API token. The token must have `Zone` > `Workers Routes` > `Edit` for the zone
   `manycommander.app`.

## Verification

1. Check the apex. The status is `200`:

   ```bash
   curl -sI https://manycommander.app/
   ```

2. In the same output, find these headers: `content-security-policy`,
   `x-content-type-options: nosniff`, `referrer-policy`, `permissions-policy` and
   `strict-transport-security`.
3. Check the redirect from www. The status is `301` and `location` is
   `https://manycommander.app/docs/?from=www`:

   ```bash
   curl -sI 'https://www.manycommander.app/docs/?from=www'
   ```

4. Check the redirect from HTTP. The status is `301` or `308`, and `location` starts with
   `https://`:

   ```bash
   curl -sI http://manycommander.app/
   ```

5. Check a page that does not exist. The status is `404`:

   ```bash
   curl -sI https://manycommander.app/no-such-page/
   ```

6. Check the mail records. The output shows `0 .`, then `"v=spf1 -all"`, then the DMARC
   record:

   ```bash
   dig +short MX manycommander.app
   dig +short TXT manycommander.app
   dig +short TXT _dmarc.manycommander.app
   ```

7. Check DNSSEC. The `flags:` line contains `ad`:

   ```bash
   dig +dnssec manycommander.app A | grep flags:
   ```

   The `ad` flag needs a resolver that validates DNSSEC. If the flag is missing, add the
   address of a public validating resolver as `@<address>` to the command.

8. Open `https://manycommander.app/` in a browser. Confirm that the page shows the
   screenshot and that the theme buttons change the colours.

## Rollback

- To stop all deploys, set the variable to `false`:

  ```bash
  gh variable set SITE_DEPLOY --body false --repo manyfold-dk/manycommander
  ```

- To go back to the previous version of the site, open **Workers & Pages** >
  `manycommander-site` > **Deployments** in the Cloudflare dashboard. Click **Rollback** on
  the previous version.
- To take the site offline, open **Workers & Pages** > `manycommander-site` > **Settings** >
  **Domains & Routes**. Remove the two custom domains.
- To rotate the API token, do Step 3 and Step 5, item 1, again. Then delete the old token
  in **Account API Tokens**. Do this at once if the token is exposed.
