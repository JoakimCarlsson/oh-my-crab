"""Checks the house rules for the Python files under src/ and lists every break of them."""

import ast
import pathlib
import sys

LIMIT = 100


def problems(path):
    """Return the problems of the file at ``path``, each as a line to print."""
    text = path.read_text()
    tree = ast.parse(text)
    found = []
    if ast.get_docstring(tree) is None:
        found.append(f"{path}:1: the module has no docstring")
    names = [
        target.id
        for node in tree.body
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name)
    ]
    if "__all__" not in names:
        found.append(f"{path}:1: the module does not define __all__")
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and ast.get_docstring(node) is None:
            found.append(f"{path}:{node.lineno}: {node.name} has no docstring")
        if isinstance(node, ast.Call) and getattr(node.func, "id", None) == "print":
            found.append(f"{path}:{node.lineno}: print is not allowed, return the text instead")
        if isinstance(node, ast.ExceptHandler) and node.type is None:
            found.append(f"{path}:{node.lineno}: a bare except is not allowed, name the exception")
    for number, line in enumerate(text.split("\n"), start=1):
        if len(line) > LIMIT:
            found.append(f"{path}:{number}: the line is longer than {LIMIT} characters")
    return found


def main():
    """Print every problem under src/ and end with 1, or print ok and end with 0."""
    found = []
    for path in sorted(pathlib.Path("src").glob("*.py")):
        found.extend(problems(path))
    if found:
        print("\n".join(found))
        print(f"{len(found)} problems")
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
