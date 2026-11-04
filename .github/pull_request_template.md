<!--
The title becomes the squash-merge commit on master and its changelog entry, so it must follow
the commit convention in cog.toml, e.g. `feat(recurrence): expand RDATE periods`.
See CONTRIBUTING.md.
-->

## Summary

<!-- What changes, and why. -->

Closes #

## Testing

<!-- The commands you ran and what they showed, and anything left untested. -->

## Checklist

- [ ] The title and every commit follow the commit convention in `cog.toml`
- [ ] `mise run ci` passes locally, or the gates it skipped are named above
- [ ] New public items have doc comments
- [ ] The pure crates stay pure: no clock, filesystem, environment or ambient randomness
- [ ] No new dependency with a licence `deny.toml` rejects, and no code copied from a copyleft project
