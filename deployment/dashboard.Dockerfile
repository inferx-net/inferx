# syntax=docker/dockerfile:1

FROM python:3.10-slim-buster

WORKDIR /

RUN apt-get -y update
RUN apt-get install -y libpq-dev gcc
RUN apt-get install -y bash
RUN apt-get install -y nginx
RUN apt-get install -y curl

COPY requirements.txt requirements.txt
RUN pip3 install -r requirements.txt

COPY . .

COPY nginx.conf /etc/nginx/sites-available/default

CMD service nginx start && gunicorn -w 4 -b 0.0.0.0:1250 app:app