import sys
from huggingface_hub import snapshot_download
import os

def main():
    if len(sys.argv) < 2:
        print("Usage: docker run ... <repo_id> [revision] [repo_type]")
        print("Example: docker run ... meta-llama/Llama-3-8B")
        sys.exit(1)

    repo_id = sys.argv[1]
    revision = sys.argv[2] if len(sys.argv) > 2 else None
    repo_type = sys.argv[3] if len(sys.argv) > 3 else "model"

    cache_dir = os.getenv("HF_HUB_CACHE", "/models/hub")
    os.makedirs(cache_dir, exist_ok=True)

    print(f"Downloading repo: {repo_id}")
    print(f"Cache dir: {cache_dir}")
    print(f"Revision: {revision}")
    print(f"Repo type: {repo_type}")

    snapshot_download(
        repo_id=repo_id,
        revision=revision,
        repo_type=repo_type,
        cache_dir=cache_dir,
    )

    print("Download completed.")

if __name__ == "__main__":
    main()
