# Politique de confidentialité de romulus

**Version :** 1.0
**Date d'entrée en vigueur :** 21 octobre 2026

La présente Politique de confidentialité explique comment les données personnelles sont traitées lorsque vous utilisez le service hébergé romulus disponible à l'adresse **https://romulus.lupidae.com** (le « **Service** ») et son site internet. Elle est fournie en application des articles 13 et 14 du règlement (UE) 2016/679 (le « **RGPD** ») et de la loi n° 78-17 du 6 janvier 1978 (la « **loi Informatique et Libertés** »). Les termes commençant par une majuscule ont le sens que leur donnent les Conditions générales d'utilisation et de vente disponibles à l'adresse https://romulus.lupidae.com/fr/legal/conditions.

La présente politique est disponible en français à l'adresse https://romulus.lupidae.com/fr/legal/confidentialite et en anglais à l'adresse https://romulus.lupidae.com/legal/privacy. **En cas de divergence, la version française prévaut.**

Le Service est réservé aux professionnels (article 3.5 des Conditions). Les personnes dont nous traitons les données sont donc principalement des personnes qui utilisent le Service dans le cadre de leur travail : travailleurs indépendants et autres professionnels agissant pour leur propre compte, ainsi que les salariés et prestataires de nos clients professionnels.

La présente politique ne couvre pas les installations du logiciel libre romulus que des tiers exploitent eux-mêmes. La personne qui exploite une telle installation est responsable des données qui y sont traitées.

## 1. Responsable du traitement

Le responsable des traitements décrits dans la présente politique est :

- **Lucas Lefort, entrepreneur individuel (EI)**, exerçant sous le nom commercial « romulus » ;
- 108 rue de Gand, 59200 Tourcoing, France ;
- SIREN 949 815 138 ;
- e-mail : **romulus@lupidae.com** ; téléphone : 07 82 98 35 07.

romulus n'a pas désigné de délégué à la protection des données : il n'y est pas tenu au regard de l'article 37 du RGPD, ses activités de base ne comportant ni suivi régulier et systématique à grande échelle des personnes, ni traitement à grande échelle de données sensibles. Pour toute question relative à vos données, écrivez à romulus@lupidae.com.

**Responsable de traitement ou sous-traitant.** romulus est **responsable de traitement** pour les données relatives aux Comptes, aux Organisations, à la facturation, à la sécurité et au support décrites ci-dessous. Lorsqu'un client intègre des données personnelles dans son propre contenu (par exemple un nom de colonne, une valeur par défaut ou un commentaire mentionnant une personne) ou dans le journal d'audit de son Organisation, romulus traite ces données en qualité de **sous-traitant** pour le compte de ce client, conformément à l'accord de traitement des données disponible à l'adresse https://romulus.lupidae.com/legal/dpa. Pour ces données, le client est responsable de traitement et doit être contacté en premier lieu.

## 2. Données traitées

romulus ne se connecte jamais aux bases de données de ses clients. Nous ne recevons pas les lignes qu'elles contiennent. Les Playgrounds contiennent de fausses données générées.

| Catégorie | Contenu | Source |
|---|---|---|
| **Données de Compte** | Adresse e-mail et indication de sa vérification, nom. Dans notre service de connexion auto-hébergé : prénom et nom, adresse e-mail, identifiant, mot de passe (conservé uniquement sous forme d'empreinte), paramètres du second facteur si vous en configurez un, et historique des sessions de connexion (date, identifiant du navigateur, adresse IP) | Vous |
| **Données de session** | Un identifiant de session aléatoire (dont nous ne conservons que l'empreinte), ses dates de création, de dernière utilisation et d'expiration | Générées par le Service |
| **Données d'Organisation** | Organisations dont vous êtes membre, leur nom, votre rôle (administrateur, développeur ou relecteur), invitations (adresse e-mail de la personne invitée, rôle proposé, auteur de l'invitation, date d'expiration) | Vous, ou l'administrateur qui vous invite |
| **Contenu client** | Définitions de schémas (DDL), brouillons, versions, migrations, commentaires (texte, mentions, nom de l'auteur), approbations (nom de l'approbateur), jetons d'API (conservés uniquement sous forme d'empreinte, avec leur date de dernière utilisation) et, dans les Playgrounds, le texte des requêtes et migrations exécutées (les 64 premiers Ko de chacune, avec le nom de la personne qui l'a lancée), jamais leurs résultats | Vous et les membres de votre Organisation, ou votre chaîne de CI au moyen d'un jeton d'API |
| **Données de collaboration** | Présence et position du curseur pendant l'édition en temps réel | Vous |
| **Journal d'audit** | Actions réalisées dans une Organisation, avec le nom de leur auteur et leur date : versions publiées, retirées, restaurées ou effacées ; approbations passées outre ; actions sur les Playgrounds ; suppression du commentaire d'une autre personne (avec les 200 premiers caractères de ce commentaire) | Générés par le Service |
| **Données contractuelles** | Votre Compte, la date, la version des Conditions acceptée, la langue, vos attestations d'usage professionnel et, pour une Offre payante, l'Offre et la Période de facturation choisies. Si nous les demandons en application de l'article 9.1 des Conditions, les informations qui confirment l'usage professionnel (telles qu'une dénomination, un numéro SIREN ou de TVA, ou le nom de votre employeur) | Vous |
| **Données de facturation** | Nom de l'Organisation, adresse e-mail de l'administrateur qui souscrit, nom et adresse de facturation, raison sociale et numéro de TVA s'ils sont renseignés, Offre, factures, statut des paiements. Nous ne recevons jamais les données complètes de votre carte : vous les saisissez sur la page de Stripe. Stripe nous communique des informations limitées sur le moyen de paiement (comme la marque de la carte, ses quatre derniers chiffres et sa date d'expiration) | Vous, via Stripe |
| **Données d'acheminement des e-mails** | Adresse du destinataire, objet, statut de remise et de rejet des e-mails envoyés par le Service | Notre prestataire d'envoi d'e-mails |
| **Données techniques et de sécurité** | Adresse IP (transmise par Cloudflare et utilisée pour limiter les requêtes abusives), méthode, chemin et identifiant des requêtes, identifiant de l'utilisateur, date et heure, erreurs | Générées par le Service et notre infrastructure |
| **Données de support et de signalement** | Vos messages et nos réponses, y compris les signalements de contenus illicites et de vulnérabilités | Vous |

Votre navigateur conserve également vos préférences d'affichage en local (article 8).

## 3. Finalités et bases légales

| Finalité | Données | Base légale (RGPD) |
|---|---|---|
| Créer et gérer votre Compte, vous authentifier | Données de Compte, de session, techniques | Exécution du contrat, art. 6.1.b |
| Fournir le Service : stockage et versionnage des schémas, collaboration en temps réel, commentaires, approbations, migrations, publications depuis la CI, Playgrounds, exports | Contenu client, données de collaboration, données d'Organisation | Exécution du contrat, art. 6.1.b |
| Gérer les Organisations, invitations et rôles | Données d'Organisation, e-mail de la personne invitée | Exécution du contrat, art. 6.1.b (voir la précision sous le tableau) ; pour les personnes invitées non encore utilisatrices, notre intérêt légitime à permettre au client d'inviter son équipe, art. 6.1.f |
| Fournir le journal d'audit à l'Organisation | Journal d'audit | En qualité de sous-traitant, pour le compte du client (accord de traitement des données) ; à défaut, exécution du contrat, art. 6.1.b |
| Envoyer les e-mails du Service (invitations, facturation, rappels de renouvellement, heures de Playground, informations sur les modifications) | Données de Compte, d'Organisation, d'acheminement | Exécution du contrat, art. 6.1.b |
| Conclure le contrat, vérifier que le Service est utilisé à des fins professionnelles et conserver la preuve du contrat | Données contractuelles | Mesures précontractuelles et exécution du contrat, art. 6.1.b ; intérêt légitime à réserver le Service aux professionnels et à prouver le contrat, art. 6.1.f |
| Facturation et comptabilité | Données de facturation | Exécution du contrat, art. 6.1.b ; obligation légale (comptable et fiscale), art. 6.1.c |
| Sécurité du Service, prévention de la fraude et des abus, respect des règles d'utilisation acceptable (y compris les abus de Playgrounds) | Données techniques et de sécurité, journal d'audit, données de Compte | Intérêt légitime à sécuriser le Service et ses utilisateurs, art. 6.1.f |
| Répondre à vos demandes et assurer le support | Données de support, données de Compte | Exécution du contrat, art. 6.1.b, ou intérêt légitime à répondre aux demandes, art. 6.1.f |
| Traiter les signalements de contenus illicites et les décisions qui en découlent | Contenu du signalement, données de son auteur et du Client concerné | Obligation légale (articles 16 et 17 du règlement (UE) 2022/2065, dit « règlement sur les services numériques »), art. 6.1.c |
| Constatation, exercice ou défense de droits en justice | Données pertinentes | Intérêt légitime, art. 6.1.f |
| Réponse aux demandes légales des autorités | Données pertinentes | Obligation légale, art. 6.1.c |
| Transfert de l'activité à une société successeur (article 4) | Données nécessaires à la poursuite du Service | Intérêt légitime à assurer la continuité de l'activité, art. 6.1.f |

**Exécution du contrat.** Lorsque le tableau indique « exécution du contrat » comme base légale, celle-ci s'applique si vous êtes vous-même le Client, par exemple en tant que travailleur indépendant. Lorsque vous utilisez le Service pour le compte de votre employeur ou d'une autre organisation qui est le Client, la base légale est notre intérêt légitime, et celui du Client, à fournir le Service auquel le Client a souscrit, art. 6.1.f.

Nous ne vendons pas de données personnelles, ne les utilisons pas à des fins publicitaires, et ne les utilisons pas pour des décisions fondées exclusivement sur un traitement automatisé produisant des effets juridiques ou des effets similaires significatifs (article 22 du RGPD). Nous n'utilisons aucun outil de mesure d'audience ou de suivi, et n'envoyons pas d'e-mails de prospection. Si cela devait changer, nous mettrions à jour la présente politique au préalable et recueillerions votre consentement lorsque la loi l'exige.

Lorsque le traitement repose sur notre intérêt légitime, vous pouvez vous y opposer à tout moment (article 7).

La fourniture de votre adresse e-mail et de votre nom est nécessaire à la création d'un Compte, et celle des données de facturation à la souscription d'une Offre payante. À défaut, nous ne pouvons pas fournir le Service.

## 4. Destinataires

Les données personnelles sont accessibles uniquement :

- à **romulus**, c'est-à-dire Lucas Lefort et les personnes qui exploitent pour son compte le Service et son infrastructure d'hébergement, toutes tenues à la confidentialité, dans la stricte mesure nécessaire à l'exploitation, à la sécurité et au support du Service ;
- aux **membres de votre Organisation**, qui voient votre nom, votre adresse e-mail, votre rôle, vos commentaires, approbations, exécutions de Playground et contributions ; les développeurs et administrateurs voient également le journal d'audit ;
- à **nos sous-traitants et prestataires**, listés ci-dessous, dans la stricte mesure nécessaire à leur prestation ;
- aux **autorités publiques**, lorsque la loi l'exige ;
- à **une société successeur** : romulus est actuellement exploité par un entrepreneur individuel et il est prévu que l'activité soit transférée à une société, envisagée sous forme de SAS de droit français contrôlée par Lucas Lefort. Dans ce cas, les données nécessaires à la poursuite du Service seront transférées à cette société, qui deviendra responsable de traitement et poursuivra le traitement conformément à la présente politique. Vous en serez informé par e-mail au moins 30 jours à l'avance.

### Sous-traitants et prestataires

| Prestataire | Prestation | Données | Lieu de traitement | Qualité |
|---|---|---|---|---|
| **Prestataire d'hébergement établi en France** (prestataire personne physique) | Exploitation de l'infrastructure qui exécute le Service, ses bases de données et ses sauvegardes | Ensemble des données traitées par le Service | France | Sous-traitant, en vertu d'un contrat écrit (article 28 du RGPD) |
| **Cloudflare, Inc.** (États-Unis) | Hébergement du site internet, point d'entrée réseau et tunnel sécurisé vers nos serveurs, DNS, acheminement des e-mails adressés à notre adresse de contact | Ensemble du trafic vers et depuis le Service, y compris les adresses IP et le contenu des requêtes ; e-mails adressés à romulus@lupidae.com | Union européenne, États-Unis et réseau mondial de Cloudflare | Sous-traitant |
| **Plus Five Five, Inc.**, qui exploite **Resend** (États-Unis) | Envoi des e-mails du Service | Adresse e-mail et nom du destinataire, contenu des e-mails (sans contenu de schéma ni commentaire), statut de remise | Union européenne (Irlande) pour l'envoi ; États-Unis pour les systèmes propres du prestataire | Sous-traitant |
| **Stripe Payments Europe, Limited** (Irlande) | Page de paiement, traitement des paiements, abonnements et factures | Données de facturation et de paiement | Union européenne et États-Unis (groupe Stripe) | Sous-traitant pour la gestion des abonnements et la facturation ; responsable de traitement indépendant pour le traitement des paiements, la prévention de la fraude et ses propres obligations légales, selon sa politique de confidentialité (https://stripe.com/fr/privacy) |
| **Notre prestataire de messagerie** | Boîte de réception des messages que vous adressez à romulus@lupidae.com | Vos messages et pièces jointes, votre adresse e-mail | Selon ses conditions ; tout transfert hors de l'EEE est encadré par les garanties de l'article 5 | Sous-traitant |

**Service de connexion.** La connexion repose sur Zitadel, logiciel libre que nous exploitons nous-mêmes sur notre propre infrastructure en France. Son éditeur ne reçoit aucune de vos données et n'est pas sous-traitant.

**GitHub.** Le code source du logiciel libre est publié sur GitHub. GitHub ne reçoit ni données de Compte ni contenu client du Service et n'est pas sous-traitant. Le Service ne propose pas de connexion via GitHub.

## 5. Transferts hors de l'Union européenne

Le Service et ses données sont hébergés en France. Certains prestataires peuvent traiter des données en dehors de l'Espace économique européen (EEE), notamment aux États-Unis (Cloudflare, Resend et le groupe Stripe).

Ces transferts sont encadrés par :

- le **cadre de protection des données UE-États-Unis** (*EU-US Data Privacy Framework*, décision d'exécution (UE) 2023/1795 de la Commission du 10 juillet 2023), lorsque le destinataire y est certifié, ce qui est le cas de Cloudflare, de Resend et de Stripe à la date de la présente politique ; et, en complément,
- les **clauses contractuelles types** adoptées par la Commission européenne (décision d'exécution (UE) 2021/914), intégrées aux accords de traitement des données de ces prestataires.

Vous pouvez obtenir une copie de ces garanties en écrivant à romulus@lupidae.com.

## 6. Durées de conservation

| Données | Durée de conservation |
|---|---|
| Données de Compte | Pendant toute la durée de vie du Compte. Supprimées dans les 30 jours de votre demande de suppression, y compris du service de connexion. Un Compte inutilisé pendant 3 ans est supprimé après un avertissement adressé par e-mail au moins 30 jours à l'avance |
| Sessions | 7 jours sans utilisation, et au plus 30 jours après la connexion ; puis supprimées |
| Données d'Organisation et contenu client | Pendant toute la durée de vie de l'Organisation. Supprimés dans les 30 jours d'une demande de suppression d'un administrateur. Une base de données supprimée est effacée au bout de 30 jours ; une version retirée au bout de 30 jours (ou à la clôture du brouillon ouvert qui repose sur elle) |
| Playgrounds | Arrêtés après 15 minutes d'inactivité, supprimés 7 jours après leur dernière utilisation. Le texte des requêtes et migrations exécutées est supprimé avec la fiche du Playground, au plus tard 30 jours après sa dernière utilisation. Les Playgrounds eux-mêmes ne sont jamais sauvegardés |
| Journal d'audit | Pendant toute la durée de vie de l'Organisation, dont il constitue l'historique |
| Invitations en attente | Valables 7 jours ; supprimées 30 jours après leur expiration ou leur révocation. Les invitations acceptées sont conservées avec le registre des membres de l'Organisation |
| Données contractuelles | Pendant toute la durée du contrat et 5 ans après son terme (délai de prescription : article 2224 du Code civil et article L110-4 du Code de commerce) |
| Données de facturation et factures | 10 ans à compter de la clôture de l'exercice (article L123-22 du Code de commerce). Notifications de paiement reçues de Stripe : 90 jours |
| Journaux techniques et de sécurité (y compris les adresses IP) | 6 mois au plus |
| Données de support et de signalement | 3 ans à compter du dernier échange, davantage si nécessaire pour un litige |
| Sauvegardes | Les sauvegardes quotidiennes sont conservées 14 jours. Les données supprimées peuvent donc subsister dans les sauvegardes jusqu'à 30 jours |
| Données nécessaires à un litige | Jusqu'à l'issue du litige et l'expiration des délais de prescription |

## 7. Vos droits

Conformément au RGPD et à la loi Informatique et Libertés, vous disposez des droits suivants :

- **droit d'accès** à vos données (art. 15 du RGPD) ;
- **droit de rectification** des données inexactes (art. 16) ; vous pouvez également modifier votre nom dans le service de connexion ;
- **droit à l'effacement** (art. 17) ;
- **droit à la limitation** du traitement (art. 18) ;
- **droit à la portabilité** des données que vous avez fournies et qui sont traitées sur la base du contrat (art. 20). Vous pouvez également exporter vous-même votre contenu depuis le Service à tout moment ;
- **droit d'opposition**, à tout moment et pour des raisons tenant à votre situation particulière, aux traitements fondés sur notre intérêt légitime (art. 21) ;
- droit de définir des **directives relatives au sort de vos données après votre décès** (article 85 de la loi Informatique et Libertés).

Pour exercer vos droits, écrivez à **romulus@lupidae.com** ou à l'adresse postale indiquée à l'article 1. Nous pouvons vous demander des informations permettant de confirmer votre identité en cas de doute raisonnable. Nous répondons dans un délai d'un mois ; ce délai peut être prolongé de deux mois pour les demandes complexes ou nombreuses, auquel cas nous vous en informons dans le premier mois.

Pour les données personnelles figurant dans le contenu ou le journal d'audit d'une Organisation, nous transmettons votre demande au client concerné, qui en est responsable de traitement, et l'aidons à y répondre.

**Réclamation.** Si vous estimez que vos droits ne sont pas respectés, vous pouvez introduire une réclamation auprès de la **CNIL** (Commission nationale de l'informatique et des libertés), 3 place de Fontenoy, TSA 80715, 75334 Paris Cedex 07, https://www.cnil.fr, ou auprès de l'autorité de contrôle de l'État membre de l'Union européenne où vous résidez ou travaillez.

## 8. Cookies et traceurs

**Nous utilisons uniquement des cookies et un stockage local strictement nécessaires au Service ou qui conservent des préférences que vous avez choisies.** Nous n'utilisons aucun cookie de mesure d'audience, de publicité, de réseaux sociaux ou autre traceur de suivi, et le Service ne charge aucun script ni aucune police de tiers.

En application de l'article 82 de la loi Informatique et Libertés, qui transpose l'article 5.3 de la directive 2002/58/CE (dite « ePrivacy »), et comme le précisent les lignes directrices de la CNIL du 17 septembre 2020, le consentement n'est pas requis pour les opérations de lecture ou d'écriture sur votre terminal strictement nécessaires à la fourniture d'un service en ligne que vous avez expressément demandé. Tous les éléments listés ci-dessous relèvent de cette exemption. **C'est pourquoi le Service n'affiche pas de bandeau de consentement aux cookies.** Vous pouvez bloquer ou supprimer ces cookies dans votre navigateur, mais vous ne pourrez alors plus vous connecter.

**Cookies**

| Nom | Finalité | Déposé par | Durée |
|---|---|---|---|
| `romulus_session` | Maintenir votre connexion | romulus (romulus.lupidae.com) | 30 jours au plus ; la session prend fin après 7 jours sans utilisation |
| `romulus_session_signin` | Protéger la procédure de connexion contre les requêtes falsifiées | romulus (romulus.lupidae.com) | 10 minutes |
| `__Host-zitadel.useragent` | Reconnaître votre navigateur dans le service de connexion, pour maintenir votre session de connexion et protéger votre Compte | Service de connexion de romulus (romulus-auth.lupidae.com) | 1 an |
| `__Host-zitadel.login.csrf` | Protéger les formulaires de connexion contre les requêtes falsifiées | Service de connexion de romulus (romulus-auth.lupidae.com) | 12 heures |
| `zitadel.login.lang` | Mémoriser la langue des pages de connexion | Service de connexion de romulus (romulus-auth.lupidae.com) | Jusqu'à la fermeture du navigateur |
| `mode` | Mémoriser l'affichage clair ou sombre des pages de connexion | Service de connexion de romulus (romulus-auth.lupidae.com) | 1 an |
| `__cf_bm`, `cf_clearance` | Déposés uniquement si la protection de Cloudflare contre les robots et les attaques se déclenche : ils distinguent les personnes des robots | Cloudflare | 30 minutes pour `__cf_bm` ; durée fixée pour la vérification pour `cf_clearance` |

**Stockage local.** L'application web et le site conservent dans le stockage local de votre navigateur, sans jamais nous les transmettre :

- le thème que vous avez choisi (`romulus-theme`) ;
- la dernière Organisation consultée (`romulus-last-org`) ;
- des préférences d'affichage : barre latérale repliée ou non, tailles des panneaux et choix de vue, position des tables dans les diagrammes (clés commençant par `romulus:`) ;
- sur la page d'accueil uniquement, le contenu de l'éditeur de démonstration (`romulus:hero:1`).

**Paiement.** Le paiement a lieu sur les pages de Stripe (checkout.stripe.com et billing.stripe.com). Les cookies qui y sont déposés relèvent de la politique de Stripe. Aucun script de Stripe n'est chargé sur les pages de romulus.

Si nous ajoutions un jour des cookies non strictement nécessaires, nous recueillerions votre consentement au préalable et mettrions à jour la présente politique.

## 9. Sécurité

Nous mettons en œuvre des mesures techniques et organisationnelles adaptées au risque, notamment :

- le chiffrement en transit (TLS) entre votre navigateur ou votre CI et Cloudflare, puis au moyen d'un tunnel chiffré entre Cloudflare et nos serveurs, qui n'acceptent aucune connexion entrante depuis Internet ;
- l'isolation de chaque Playground dans un bac à sable gVisor sans accès réseau, avec des limites de processeur, de mémoire, de disque et de durée ;
- la conservation des identifiants de session et des jetons d'API uniquement sous forme d'empreinte, et des mots de passe uniquement sous forme d'empreinte par le service de connexion ;
- un contrôle d'accès par rôles au sein des Organisations, et un accès administrateur à l'infrastructure limité à un nombre restreint de personnes nommément désignées et tenues à la confidentialité ;
- des sauvegardes quotidiennes conservées 14 jours, avec des tests de restauration ;
- la journalisation des événements de sécurité, et un journal d'audit des actions sensibles dans chaque Organisation.

Le détail figure à l'annexe 2 de l'accord de traitement des données. Si une violation de données personnelles est susceptible d'engendrer un risque pour vos droits et libertés, nous la notifions à la CNIL dans les 72 heures après en avoir pris connaissance et, lorsque le risque est élevé, nous vous en informons dans les meilleurs délais (articles 33 et 34 du RGPD).

## 10. Mineurs

Le Service est réservé aux professionnels, et vous devez avoir au moins 18 ans pour créer un Compte. Nous ne collectons pas sciemment de données concernant des mineurs. Si nous apprenons qu'un Compte appartient à un mineur, nous le supprimons. Écrivez à romulus@lupidae.com si vous pensez que c'est le cas.

## 11. Modifications de la présente politique

Nous pouvons mettre à jour la présente politique, par exemple lors de l'ajout d'un sous-traitant ou du transfert de l'activité à une société successeur. Le numéro de version et la date figurant en tête identifient la version en vigueur. Nous vous informons des modifications substantielles par e-mail avant leur entrée en vigueur, et au moins 30 jours à l'avance en cas d'ajout d'un sous-traitant.

## 12. Contact

- Lucas Lefort EI (romulus), 108 rue de Gand, 59200 Tourcoing, France
- E-mail : **romulus@lupidae.com**
- Téléphone : 07 82 98 35 07
