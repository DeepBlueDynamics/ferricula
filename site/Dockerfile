FROM nginx:alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY index.html /usr/share/nginx/html/index.html
COPY docs.html /usr/share/nginx/html/docs.html
COPY ferricula.png /usr/share/nginx/html/ferricula.png
COPY kord.png /usr/share/nginx/html/kord.png
COPY tools/ /usr/share/nginx/html/tools/
EXPOSE 8080
CMD ["nginx", "-g", "daemon off;"]
