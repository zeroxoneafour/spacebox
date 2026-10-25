# file structures

All of the save data/asset files are represented as toml files. This allows the game to be extremely moddable.

The toml files are designed to be combined together. At the top of each file can be an `[import]` section with relative imports, like -

```
[import]
resources = "./resources.toml" # or simply "resources.toml" since its in the same directory
```

or the like. You can later reference a resource (in an appropriate slot) with something like `output_resource = "resources.alloy"`.

Note that each of the subsections below is designed to be combined with each other, and may all appear more than once in a file (especially a save file). However, it is very important to note that the `[import]` section may only appear once (and may not appear) at the top of the file.

The engine should automatically figure out which table to take a value from. For example, if resources.toml is imported as "abc", and some `output_resource[0].resource = "abc.def"`, since the engine knows `resource` must take in a value of resource, it looks for a `[resource.def]` in resources.toml.
That is to say, everything should be implicitly strongly-typed even in toml.

Imports should not recurse, i.e. you should not be able to reference things in a file that is imported by your import unless you also import that file.
