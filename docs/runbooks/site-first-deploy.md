# Deploy manycommander.app for the first time

This runbook connects the site workflow (`.github/workflows/site.yml`) to Cloudflare. The
workflow deploys the Worker `manycommander-site` from `main`. The deploy job runs only when
the repository variable `SITE_DEPLOY` is `true`. Do these steps one time, from a computer
that has access to the Cloudflare account.

## Prerequisites

- Access to the Cloudflare account that holds the zone `manycommander.app`. The zone
  status is `Active`.
- Admin access to the GitHub repository `manyfold-dk/manycommander`.
- The `gh` command, logged in to GitHub. The GitHub web interface also works.
- The `curl`, `dig` and `delv` commands (package `bind` on Arch Linux).

## Pre-action checklist

- [ ] The last `site` workflow run on `main` is green, and the `deploy` job shows `skipped`.
- [ ] The Cloudflare dashboard shows the zone `manycommander.app` as `Active`.
- [ ] You have a password manager or a similar place for the API token. The token goes
      only into that place and into the GitHub secret.

## Procedure

### Step 1: Check the DNS records on the apex and on www

The Worker gets the hostnames `manycommander.app` and `www.manycommander.app` as custom
domains. Cloudflare creates the DNS records for the custom domains. Cloudflare does not
create a custom domain on a hostname that has a `CNAME` record.

1. In the Cloudflare dashboard, open the zone `manycommander.app`.
2. Open **DNS** > **Records**.
3. Find the records with the name `manycommander.app` (shown as `@`) or `www`.
4. Write down each `A`, `AAAA` and `CNAME` record on these two names.
5. Delete each `A`, `AAAA` and `CNAME` record on these two names.

`MX` and `TXT` records on `@` do not conflict with the custom domains. Keep them.

### Step 2: Turn on HTTPS redirects

1. Open **SSL/TLS** > **Edge Certificates**.
2. Set **Always Use HTTPS** to on.

### Step 3: Enable DNSSEC

1. Open **DNS** > **Settings**.
2. Click **Enable DNSSEC**.
3. If the registrar is Cloudflare Registrar, Cloudflare adds the DS record. Go to item 6.
4. If the registrar is not Cloudflare, copy the DS record values from the dialog.
5. Add the DS record at the registrar of `manycommander.app`.
6. Wait until the Cloudflare dashboard shows DNSSEC as `Active`. This can take some hours.

### Step 4: Add the records for a domain that sends no mail

The domain sends no mail and receives no mail. These records tell mail servers to refuse
mail that uses the domain.

1. Open **DNS** > **Records**.
2. Add a record: type `MX`, name `@`, mail server `.`, priority `0`.
3. Add a record: type `TXT`, name `@`, content `v=spf1 -all`.
4. Add a record: type `TXT`, name `_dmarc`, content `v=DMARC1; p=reject; sp=reject; adkim=s; aspf=s`.

If Step 1 found an `MX` record or an SPF `TXT` record on `@`, delete the old record. A
domain has one SPF record only.

### Step 5: Create the API token

> **Warning:** The token gives write access to the Workers of the account. Do not put the
> token in a file, a chat or a terminal command. Paste the token only into the password
> manager and into the GitHub secret.

1. In the Cloudflare dashboard, open **My Profile** > **API Tokens**.
2. Click **Create Token**.
3. Find the template **Edit Cloudflare Workers** and click **Use template**.
4. Set **Account Resources** to `Include` and the account that holds `manycommander.app`.
5. Set **Zone Resources** to `Include`, `Specific zone` and `manycommander.app`.
6. Click **Continue to summary**.
7. Click **Create Token**.
8. Copy the token into the password manager. Cloudflare shows the token one time only.

### Step 6: Find the account ID

1. In the Cloudflare dashboard, open **Account home**.
2. Open the menu of the account and click **Copy account ID**.

The account ID is not secret, but it identifies the account. Put the account ID only into
the GitHub secret, not into the repository.

### Step 7: Add the GitHub secrets and the variable

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

### Step 8: Run the deploy

1. Start the workflow on `main`:

   ```bash
   gh workflow run site.yml --repo manyfold-dk/manycommander --ref main
   ```

2. Follow the run:

   ```bash
   gh run watch --repo manyfold-dk/manycommander "$(gh run list --repo manyfold-dk/manycommander --workflow site.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
   ```

3. Confirm that the `build` job and the `deploy` job are green.
4. If the `deploy` job fails with an authentication error on a custom domain, edit the API
   token. Add the permission `Zone` > `DNS` > `Edit` for the zone `manycommander.app`.
   Then do this step again.

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

7. Check DNSSEC. The first line of the output is `; fully validated`:

   ```bash
   delv manycommander.app
   ```

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
- If the API token is exposed, open **My Profile** > **API Tokens** in the Cloudflare
  dashboard. Click **Roll** on the token. Then do Step 7, item 1, with the new token.
