# Website review -- 29 September 2026

Reviewed the source at `e221a5fee6280bb9391a1dfb9e441acd51f0764a`, matching freshly
fetched `origin/main`, and the deployed site at <https://manycommander.app>.
This review updates the assessment from 28 September after the phase 2 and phase 3 features
were released. It assesses the website, not the correctness of the application features.

Contents: [Assessment](#assessment) · [Findings](#findings) ·
[Design direction](#design-direction) · [Verification](#verification)

## Assessment

**The product has outgrown the website's hierarchy.** The new features make manycommander
considerably more useful, but the homepage still gives most of its attention to themes and
basic file operations.

The terminal styling remains a good fit. JetBrains Mono, panel borders and the theme
preview give the site a coherent identity connected to the application.

The previous public-access blocker is resolved: the repository now returns HTTP 200, and
the phase 3 release is publicly available. See the [changelog](../../CHANGELOG.md) and
[GitHub releases](https://github.com/manyfold-dk/manycommander/releases).

The new documentation is the strongest improvement. It explains editing remote files,
retaining changed archive copies, preview compatibility and unsupported server operations.
Those explanations build confidence. The new screenshots also provide material the homepage
should use.

## Findings

| Priority | Finding and evidence | Recommended change |
|---|---|---|
| High | **The safety promise now contradicts the SFTP documentation.** The [hero](../../site/templates/index.html) still says copies and moves "never leave a half-written file behind." The [SFTP guarantees](../../site/content/docs/file-operations.md#sftp-uploads-and-changes-on-a-server) correctly explain that servers without hard links receive direct writes, and a lost connection can leave partial data. The local direct-write fallback also remains an exception, as described in the [normative design](../specs/implemented/2026-09-27-manycommander-design.md#41-invariants). | Scope homepage guarantees explicitly to the operations and filesystems that provide them. Give remote-transfer limitations a short, visible explanation near the SFTP feature. |
| High | **The installation page misses the released binary.** A Linux x86-64 archive and checksum are available in GitHub releases, but the [installation page](../../site/content/docs/install.md) presents Rust as a prerequisite and installation from Git as the main route. That command follows the repository rather than a named release. | Offer "Download for Linux x86-64" with installation and checksum instructions. Keep building from source as another option. Distinguish released builds from development builds. |
| Medium | **The strongest new features are buried.** At a 1440px viewport width, both new cards begin around 1816px down the page. At 390px, the archives/previews/SFTP card starts around 2793px down. Three major capabilities share one paragraph in the [homepage template](../../site/templates/index.html). | Introduce archives, image previews, remote transfers and bulk rename near the top. Give each a concrete benefit and visual example. |
| Medium | **Documentation navigation no longer fits its content.** At a 390px viewport width, the expanded sidebar occupies 342px, putting the Quick view article title around 484px down. The footer requires 1543px inside a 1440px viewport. See the [sidebar](../../site/templates/sidebar.html), [footer](../../site/templates/base.html) and [styles](../../site/static/css/site.css). | Collapse mobile navigation. Group desktop navigation into getting started, workflows and reference. Replace the single scrolling footer row with wrapping groups. |
| Medium | **Long reference pages need navigation within the page.** [Find and rename](../../site/content/docs/find-and-rename.md) exceeds 2100 words and covers jumping, filtering, searching, renaming and comparing. The [page template](../../site/templates/page.html) provides no contents list. | Add an "On this page" list and copyable heading links. Consider splitting Find, Multi-rename and Compare into focused pages. |

## Design direction

Make the main showcase switch between file management, image preview, archive browsing,
SFTP and bulk rename. Keep theme selection as a secondary control. This would demonstrate
the expanded product immediately while preserving the terminal-inspired visual identity.

Use the screenshots already available under `site/static/screens/`. Give visitors a way to
enlarge them on mobile: on the Quick view page, a 1240px-wide screenshot is displayed at
approximately 325px wide, making interface text difficult to read.

The earlier presentation issues remain because the styles and templates are largely
unchanged:

| Issue | Evidence | Recommendation |
|---|---|---|
| Theme controls dominate the early page | All 22 named themes, plus the system option, precede the feature cards. | Show a few representative choices and an expandable remainder. |
| Accent text has insufficient contrast in several palettes | Accent/background ratios remain 4.34:1 for Catppuccin Latte, 3.86:1 for Miasma and 3.14:1 for Rose Pine. Palette data is unchanged from the previous review. Latte is the default light palette. | Introduce website-specific text and selected-label colours with sufficient contrast. Preserve the original palette in application screenshots. |
| Installation command wraps awkwardly | The desktop hero splits the `--root` argument across lines. | Widen the command container or wrap at sensible boundaries. Place prerequisites beside the first installation action. |
| Sidebar word counts add little navigational value | Labels such as `2139w` occupy space beside page names. | Remove the counts and use the space for clearer navigation. |
| Shared links lack a preview image | The [base template](../../site/templates/base.html) has Open Graph text metadata but no image. | Add an Open Graph image showing the application. |

For ordinary text, the contrast target is at least 4.5:1 under
[WCAG AA](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html).

## Verification

| Check | Result |
|---|---|
| Source revision | Local HEAD matched freshly fetched `origin/main` at the revision above. |
| Public source and release | Repository returned HTTP 200. Release metadata listed the Linux x86-64 archive and its checksum. |
| Live pages | All 13 site pages fetched successfully. |
| Internal fragment links | All 55 checked links resolved to existing anchors. |
| New screenshot assets | Quick view, SFTP and archive screenshots returned HTTP 200. |
| Deployed assets | CSS, JavaScript and theme palette data matched the local files. |
| Browser inspection | Inspected the desktop homepage and mobile homepage/documentation layouts. No browser console errors appeared. |
| Local site build | `scripts/site.sh check` could not run because Zola was unavailable. |
| Scope limit | This was a website review, not validation of application features or a complete accessibility audit. |

The review itself changed no application or website files.
