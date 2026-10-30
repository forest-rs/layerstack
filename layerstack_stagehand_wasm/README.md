# layerstack_stagehand_wasm

The WebAssembly module of the TypeScript package
`@forest-rs/layerstack_stagehand` (`packages/layerstack_stagehand`): the
mesh plans and implicit shapes of `layerstack_stagehand`, over typed
arrays. It does not link Layerstack: the package reads a stage through
`@forest-rs/layerstack` and hands the arrays over.

The exports are an ABI for the TypeScript wrapper, which is the API.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
