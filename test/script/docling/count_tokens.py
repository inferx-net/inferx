#!/usr/bin/env python3
"""
Count tokens in one or more markdown files using Qwen3.6-35B-A3B-FP8 tokenizer.

Uses the Qwen tokenizer from HuggingFace, which is the same tokenizer used by
the Qwen3.6-35B-A3B-FP8 model served via inferx.

Usage:
    python count_tokens.py file1.md [file2.md ...]
    python count_tokens.py merged.md llm.md
    cat merged.md | python count_tokens.py -
    python count_tokens.py --model Qwen/Qwen3.6-35B-A3B-FP8 path/to/docs/

Files are merged with a separator before tokenization.
"""

import sys
import argparse
from pathlib import Path


def get_tokenizer(model_name: str = "Qwen/Qwen3.6-35B-A3B-FP8"):
    """Load the Qwen tokenizer from HuggingFace."""
    from transformers import AutoTokenizer
    return AutoTokenizer.from_pretrained(model_name, trust_remote_code=True)


def count_tokens(text: str, tokenizer) -> dict:
    """Tokenize text and return token counts."""
    tokens = tokenizer.encode(text, add_special_tokens=False)
    return {
        "text_length": len(text),
        "num_tokens": len(tokens),
        "avg_chars_per_token": len(text) / len(tokens) if tokens else 0,
    }


def main():
    parser = argparse.ArgumentParser(
        description="Count tokens in markdown files using Qwen3.6 tokenizer"
    )
    parser.add_argument(
        "files",
        nargs="+",
        help="Markdown files to count (or '-' for stdin)",
    )
    parser.add_argument(
        "--model",
        default="Qwen/Qwen3.6-35B-A3B-FP8",
        help="HuggingFace model name for tokenizer (default: Qwen/Qwen3.6-35B-A3B-FP8)",
    )
    parser.add_argument(
        "--sep",
        default="\n\n---\n\n",
        help="Separator between files (default: '\\n\\n---\\n\\n')",
    )
    parser.add_argument(
        "--include-seps",
        action="store_true",
        help="Count separator content as tokens",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Show per-file breakdown",
    )
    args = parser.parse_args()

    # Load tokenizer (cached after first run)
    print(f"Loading tokenizer: {args.model}")
    tokenizer = get_tokenizer(args.model)
    print(f"  vocab_size: {tokenizer.vocab_size}")
    print(f"  model_max_length: {tokenizer.model_max_length:,}")
    print()

    # Read all files
    file_contents = []
    file_paths = []
    for f in args.files:
        if f == "-":
            content = sys.stdin.read()
            file_paths.append("<stdin>")
        else:
            path = Path(f)
            if not path.exists():
                print(f"ERROR: File not found: {f}", file=sys.stderr)
                sys.exit(1)
            content = path.read_text(encoding="utf-8")
            file_paths.append(f)
        file_contents.append(content)

    results = []
    for path, content in zip(file_paths, file_contents):
        result = count_tokens(content, tokenizer)
        result["file"] = path
        results.append(result)

    # Print per-file breakdown if verbose or multiple files
    if args.verbose or len(results) > 1:
        print(f"{'File':<50} {'Tokens':>10} {'Chars':>10} {'C/T':>6}")
        print("-" * 80)
        for r in results:
            print(f"{r['file']:<50} {r['num_tokens']:>10,} {r['text_length']:>10,} {r['avg_chars_per_token']:>6.1f}")
        print()

    # Merge all content and count total
    if args.include_seps:
        merged = args.sep.join(file_contents)
        sep_token_count = count_tokens(args.sep * (len(file_contents) - 1), tokenizer)["num_tokens"]
    else:
        merged = "".join(file_contents)
        sep_token_count = 0

    total = count_tokens(merged, tokenizer)

    print("=" * 80)
    print(f"TOTAL: {total['num_tokens'] + sep_token_count:,} tokens")
    print(f"       {total['text_length']:,} chars")
    sep_pct = sep_token_count / (total['num_tokens'] + sep_token_count) * 100 if (total['num_tokens'] + sep_token_count) else 0
    print(f"       sep content {sep_token_count:,} tokens ({sep_pct:.1f}%)" if args.include_seps else "")
    print(f"       ~{total['avg_chars_per_token']:.1f} chars/token")
    print()
    print(f"Estimated cost at ~$20/1M tokens: ${(total['num_tokens'] + sep_token_count) * 20 / 1_000_000:.2f}")
    print()

    # Estimate context window usage
    model_max = tokenizer.model_max_length
    pct = ((total['num_tokens'] + sep_token_count) / model_max) * 100
    if pct < 90:
        status = "OK"
    elif pct < 99:
        status = "WARNING"
    else:
        status = "OVERFLOW"
    print(f"Context window: {total['num_tokens'] + sep_token_count:,} / {model_max:,} ({pct:.1f}%) [{status}]")

    return total


if __name__ == "__main__":
    main()
