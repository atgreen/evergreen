# Your first program

In this tutorial you will write a small expense report and run it as a script.
You need a working `torcl` command on your `PATH`; if you do not have one,
[build and install it first](../how-to/build.md).

## Evaluate an expression

In a terminal, run:

```sh
torcl --no-init --eval '(+ 12 8 5)'
```

TorCL prints `25` and exits.

## Define a function in the REPL

Start the interactive read–eval–print loop:

```sh
torcl --no-init
```

At the `CL-USER>` prompt, enter:

```lisp
(defun total (amounts)
  (reduce #'+ amounts :initial-value 0))

(total '(12 8 5))
```

The function definition returns its name. The next expression returns `25`.
You can call the function with another list without redefining it:

```lisp
(total '(4 6))
```

This returns `10`. Enter `(quit)` to leave the REPL.

## Put the program in a file

Create `expenses.lisp` with this content:

```lisp
(defun total (amounts)
  (reduce #'+ amounts :initial-value 0))

(format t "~A: ~D~%"
        (first *command-line-args*)
        (total '(12 8 5)))
```

Run the file, passing a report label after `--`:

```sh
torcl expenses.lisp -- Lunch
```

The program prints:

```text
Lunch: 25
```

You now have a Lisp script that takes input from the command line and produces
formatted output. To distribute an executable containing your loaded program,
continue with [Save an executable](../how-to/save-executable.md).
