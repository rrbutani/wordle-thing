# `wordle-thing`

silly program that tries to guess your usual first guess from a tweet thread

---

Call with: `cargo run -- <id of a thread thread>`.

For [example](https://twitter.com/KZXcellent/status/1481194168605097987): `cargo run -- 1481194168605097987`:
```bash
[222] ⬛🟨⬛⬛🟨 (mount)
[221] ⬛⬛⬛⬛⬛ (whack)
[220] 🟩⬛⬛🟨⬛ (sugar)
[219] ⬛⬛⬛⬛🟨 (knoll)
[218] ⬛⬛⬛🟨⬛ (crimp)
[217] ⬛⬛🟨⬛🟨 (wince)
[216] ⬛⬛⬛🟨⬛ (prick)
Using regex: `[s][t][e][r][n]`.

Is your first guess.. stern?
```
