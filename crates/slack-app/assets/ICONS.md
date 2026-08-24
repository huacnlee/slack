# Icons

These are [Lucide](https://lucide.dev) icons, taken unmodified from
`lucide-static` and reformatted onto one line. Lucide is ISC licensed.

The shared set in `gpui-component-assets` is also Lucide; the ones here are the
names that set does not ship. Keeping both from the same family is what stops
the interface from looking like two products.

To add one:

```sh
curl -fsSL https://unpkg.com/lucide-static@latest/icons/<name>.svg \
  | sed '/@license/d' | tr -s ' \n' ' ' > icons/<name>.svg
```

Then add a variant to `SlackIcon` in `crates/slack-ui/src/icons.rs`.
