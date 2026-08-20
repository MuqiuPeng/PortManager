# Wire fixtures

`wire.json` is written by `crates/runtime-types/examples/shapes.rs`, not by
hand. Regenerate it with `pnpm fixtures` after changing a view type.

The reason it is generated: the frontend's idea of a payload is a TypeScript
interface somebody wrote from memory, and the one that took the window down was
wrong in the only way that matters. It said `ports` was always present;
`skip_serializing_if = "Vec::is_empty"` leaves it out when empty; reading
`.length` off the gap threw during render and React unmounted the whole tree.
A blank window, no error dialog, nothing in the console.

A fixture copied from the wire cannot make that mistake, so the tests that use
it fail the moment a component assumes more than the daemon promises.
