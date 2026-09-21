# Legacy oracle modules

Verbatim copies of the original `fly-plays-pokemon` neural modules (commit 9ad160b plus the
uncommitted 2026-09-14 ratchet-sprint working tree). Only import paths were changed.

They exist so the generalized modules in `src/` can be proven bit-exact against the original
behaviour under default configuration. Never edit them; if the kernel must change, bump the
kernel/plasticity version string instead.
