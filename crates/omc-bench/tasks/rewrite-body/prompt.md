In `slug.py`, replace the whole body of `slugify` below its docstring with exactly this, indented as a function body. Keep the signature and the docstring, and change nothing else.

```python
text = unicodedata.normalize("NFKD", text)
text = text.encode("ascii", "ignore").decode("ascii")
words = re.findall(r"[a-z0-9]+", text.lower())
slug = separator.join(words)
return slug[:limit].rstrip(separator) if limit else slug
```
