# infra/env — the env files, and why the real ones are not here

`infra/env/example.env` is the only env file in this repo. It lists every knob the
`infra/` scripts read, with placeholder values and the comments that say what each one
costs. It is a template: it will not deploy anything.

**The real env files live in the operator's infra repo, not here.** This repo is public,
and a real env file names the host, the container id, the LAN address, the Twitch channel
and the `pass` entry that holds the stream key. Keeping them out is the point of the
de-PII sprint (inventory in the operator's infra repo); `infra/tests/lint.sh` refuses those
patterns on every run.

## Handing a script an env file from outside the repo

Every script takes the env file as its **first argument**, and that argument may be any
absolute or relative path — nothing resolves it against this directory:

```sh
# on the host, as root
install -d -m 0750 /etc/fly/env          # or wherever you keep them
# ... write the real file to /etc/fly/env/fly-pokemon.env ...

infra/provision.sh /etc/fly/env/fly-pokemon.env
infra/05-deploy.sh /etc/fly/env/fly-pokemon.env <release-tarball>
infra/verify.sh    /etc/fly/env/fly-pokemon.env
```

Or set `FLY_ENV_DIR` once and pass a bare name; `load_env` resolves an argument that is
not itself a readable file against that directory:

```sh
export FLY_ENV_DIR=/etc/fly/env
infra/provision.sh fly-pokemon.env
infra/verify.sh    fly-pokemon.env
```

`FLY_ENV_DIR` is only a convenience for typing. If the file is not found either way the
script dies naming both paths it tried.

## Keeping a real env in step with this template

`example.env` changes whenever a knob is added. After pulling a flybrain bump:

```sh
diff <(grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' infra/env/example.env | sort -u) \
     <(grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' /etc/fly/env/fly-pokemon.env | sort -u)
```

Lines only on the left are knobs the real file has not been told about yet. `load_env`
requires `CTID`, `HOSTNAME`, `IP`, `GAME` and `PUSH_TARGET`; `ROLE` defaults to `dev`.

## What must never be in an env file

Secrets. `PASS_KEY` holds the **name** of a `pass` entry, never a key. The Twitch app
id/secret arrive as the systemd credential `twitch-app`, installed by
`infra/06-secrets.sh` from `pass` on the operator box; the OAuth tokens live in the
container as `/var/lib/flybridge/tokens.json`, mode 0600. `ROM_SHA256` is a digest, and
the ROM it describes is never committed, copied into the repo, or shown on stream.
