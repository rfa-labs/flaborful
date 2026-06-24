FROM pgvector/pgvector:pg16

# Standard Fly Postgres entrypoint setup
RUN apt-get update && apt-get install -y curl && rm -rf /var/lib/apt/lists/*

# Fly's Postgres machine uses /data for persistent storage
ENV PGDATA=/data/postgresql
