# docker build -t vllm-openai-upgraded .
FROM vllm/vllm-openai:v0.7.3
WORKDIR /
# Upgrade the transformers library
RUN apt-get -y update
RUN apt-get install libglib2.0-0 -y
RUN apt-get install libgl1 -y

RUN pip install --upgrade transformers
RUN pip install --upgrade safetensors
RUN pip install diffusers --upgrade
RUN pip install invisible_watermark accelerate 

COPY run_model.py /usr/lib/run_model.py
COPY run_llava.py /usr/lib/run_llava.py
COPY run_stablediffusion.py /usr/lib/run_stablediffusion.py

