.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'

# Docker build test with Rust 1.95
docker-test: ## Build and test Docker image with Rust 1.95
	docker build -f docker/Dockerfile.test --progress=plain docker/

docker-test-no-cache: ## Build Docker image without cache
	docker build --no-cache -f docker/Dockerfile.test --progress=plain docker/

# Run the container (after docker-test succeeds)
docker-run: ## Run the nuwax-codex-acp container
	docker run --rm -it $(IMAGE_TAG) --version

# Clean up
docker-clean: ## Remove Docker build cache and images
	docker builder prune -f

# Show latest build info
docker-images: ## List Docker images related to this project
	docker images | grep nuwax-codex-acp || echo "No images found"
