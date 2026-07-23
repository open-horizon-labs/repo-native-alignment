FROM python:3.13 AS builder
RUN python -m compileall /app
FROM builder AS runtime
EXPOSE 8080
