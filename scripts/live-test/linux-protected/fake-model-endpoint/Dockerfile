FROM python:3.12-slim
RUN useradd --system --uid 10001 --no-create-home app
WORKDIR /app
COPY server.py /app/server.py
USER 10001
CMD ["python3","/app/server.py"]
