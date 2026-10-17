
clone moderne de multitail 

- inclure la logique de tail (multitail c'est un wrapper au dessus de tail, ça ne doit pas être le cas ici)
- je voudrais que cette logique de tail soit éxtensible ; multitail ne permet de tail que des fichiers ; moi je veux qu'on puisse tailer des streams, par exemple :
  - logs de dockers
  - logs de docker-compose
  - logs de pods kubernetes
  - etc (propose moi des idées)

UI :
- sur l'ui, évidement, il faut pouvoir afficher plusieurs panels de logs ; heurisitique de base c'est du split horizontal; mais par la suite on peut imaginer d'autres layouts
- coloration syntaxique
- filtres par regex, en inclusion ou exclusion

Raccourcis claviers pour commencer :
- quitter
- vider tous les buffers
- mettre un buffer en "plein écran"
- pleins d'autres par la suite
