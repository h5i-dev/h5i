import ast
import sys
import os

def to_sexp(node):
    """Recursively converts an AST node into an S-expression string."""
    if isinstance(node, ast.AST):
        fields = []
        for name, value in ast.iter_fields(node):
            # Skip fields that don't affect logical structure (like lineno, col_offset)
            if name in ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', 'ctx'):
                continue
                
            if isinstance(value, list):
                if value:
                    # Convert list of nodes: (name (child1) (child2))
                    items = " ".join(to_sexp(item) for item in value)
                    fields.append(f"({name} {items})")
            elif isinstance(value, ast.AST):
                # Convert single node: (name (child))
                fields.append(f"({name} {to_sexp(value)})")
            elif value is not None:
                # Convert primitive value: (name 'value')
                fields.append(f"({name} {repr(value)})")
        
        inner = " ".join(fields)
        return f"({node.__class__.__name__}{' ' + inner if inner else ''})"
    return repr(node)

def main():
    if len(sys.argv) < 2:
        print("Usage: h5i-py-parser <file_path>")
        sys.exit(1)

    file_path = sys.argv[1]
    if not os.path.exists(file_path):
        sys.exit(1)

    with open(file_path, "r", encoding="utf-8") as f:
        source = f.read()

    try:
        # Parse the source into a stable AST tree
        tree = ast.parse(source)
        # Output the S-expression for h5i to hash and store
        print(to_sexp(tree))
    except SyntaxError:
        sys.exit(1)

if __name__ == "__main__":
    main()