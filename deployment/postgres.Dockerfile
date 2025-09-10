# syntax=docker/dockerfile:1

FROM postgres:14.5
WORKDIR /
RUN apt-get update && apt-get install -y vim

COPY . .

RUN chmod +x /postgres-entrypoint.sh

ENTRYPOINT ["bash", "/postgres-entrypoint.sh"]
CMD ["postgres"]