## Owner steps for the production switch

`deploy-docs` publishes `/docs/` and `/schemas/` to Bunny storage on every push to `main`. It needs the `ocx.sh` environment and its secret first. Do the steps in this order.

1. Before merging, create the GitHub environment `ocx.sh` on `ocx-sh/ocx`, restricted to `main`, with the secret `BUNNY_STORAGE_KEY`.
2. In `ocx-website`, run `task bunny:onboard -- ocx-sh/ocx` to create the storage claim for this repository.
3. Merge this PR. The `deploy-docs` run on `main` uploads the site.
4. Check the storage listing for `/docs/` and `/schemas/`.
5. Remove `/docs/` and `/schemas/` from `infra/bunny/legacy.json` in `ocx-website`. Remove `/casts/`, `/assets/`, `/icons/`, `/licensed/`, `/team/` and the root files too, once the root site covers them.
6. Run `task bunny:apply` against dev, then against prod.
7. Run `cutover:verify`.
8. Retire the Cloudflare Pages project.

Merging before step 1 is safe. `deploy-docs` fails on the missing secret, nothing deploys, and the live site keeps serving the last Cloudflare Pages build.
