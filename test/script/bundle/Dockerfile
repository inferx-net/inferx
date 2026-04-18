FROM inferx/vllm-openai:v0.16.0

# Install Nginx and Supervisor
RUN apt-get update && apt-get install -y nginx supervisor curl && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy the generator script and entrypoint
COPY gen_config.py /app/gen_config.py
COPY entrypoint.sh /app/entrypoint.sh
COPY nginx.conf /etc/nginx/nginx.conf

# We no longer COPY a static supervisord.conf because it is generated at runtime
# But we need a directory for it
RUN mkdir -p /etc/supervisor/conf.d/

# Set the entrypoint to our script
ENTRYPOINT ["bash", "/app/entrypoint.sh"]

# Expose port 8000 for Nginx
EXPOSE 8000
