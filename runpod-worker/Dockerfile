FROM axolotlai/axolotl-cloud:main-py3.11-cu124-2.6.0

RUN pip install --no-cache-dir "runpod~=1.7.0" boto3

WORKDIR /app
COPY handler.py .

ENV PYTHONUNBUFFERED=1

CMD ["python", "-u", "handler.py"]
