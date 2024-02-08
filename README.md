
HTTP server which compiles Entropy programs

Usage:

Start the http server with:
`cargo run`

Then pipe a program's source code to it with `tar` and an http client (here I am using [httpie](https://httpie.io)):

```bash
cd some_example_program
tar cvf - . | http post localhost:3000/build
```
