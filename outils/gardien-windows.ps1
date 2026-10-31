# JARVIS — gardien du nœud Kynovara (Windows, nœud dans Docker).
#
# Lancé toutes les 5 minutes par la tâche planifiée « JARVIS - gardien Kynovara ».
# Vérifie : Docker, le conteneur (redémarré au besoin), la réponse du nœud, le
# minage, l'arrivée de nouveaux blocs, les signatures, l'alerte de points de
# contrôle et l'échéance de la clé de signature. Prévient par une notification
# Windows quand un problème apparaît ou disparaît (rappel toutes les 6 heures
# pour un problème grave), tient un journal (gardien.log) et un état lisible
# par JARVIS ou Claude (etat.json), à côté de ce script.
#
# « -Test » envoie en plus une notification d'essai.
param([switch]$Test)

$ErrorActionPreference = 'Continue'
$docker = 'C:\Program Files\Docker\Docker\resources\bin\docker.exe'
$conteneur = 'kynovara-testnet'
# Nœud public (serveur en ligne) auquel ce nœud se relie ; '' pour ne pas le surveiller.
$serveurPublic = 'vps-47b809cd.vps.ovh.ca'
$portPublic = 19733
$blocsParJour = 720            # un bloc toutes les 2 minutes
$sansBlocMax = 1800            # secondes sans nouveau bloc avant d'avertir
$rappelGrave = 21600           # secondes entre deux rappels d'un problème grave

$dossier = Split-Path -Parent $MyInvocation.MyCommand.Path
$etatFichier = Join-Path $dossier 'etat.json'
$journal = Join-Path $dossier 'gardien.log'
$maintenant = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()

function Journal([string]$m) {
    if ((Test-Path -LiteralPath $journal) -and (Get-Item -LiteralPath $journal).Length -gt 1MB) {
        Move-Item -LiteralPath $journal -Destination "$journal.ancien" -Force
    }
    Add-Content -LiteralPath $journal -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $m) -Encoding UTF8
}

function Notifier([string]$titre, [string]$texte) {
    try {
        [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
        [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
        $t = [System.Security.SecurityElement]::Escape($titre)
        $x = [System.Security.SecurityElement]::Escape($texte)
        $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
        $xml.LoadXml("<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$x</text></binding></visual></toast>")
        $app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
        $toast = New-Object Windows.UI.Notifications.ToastNotification $xml
        [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show($toast)
    } catch {
        Journal "notification impossible : $($_.Exception.Message)"
    }
}

function PortOuvert([string]$hote, [int]$port) {
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $ok = $client.ConnectAsync($hote, $port).Wait(4000) -and $client.Connected
        $client.Close()
        return $ok
    } catch { return $false }
}

function Rpc([string]$chemin) {
    $sortie = & $docker exec $conteneur kynovara-cli --datadir /data rpc $chemin 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $sortie) { return $null }
    try { return ($sortie | Out-String | ConvertFrom-Json) } catch { return $null }
}

$problemes = [ordered]@{}
function Probleme([string]$code, [bool]$grave, [string]$texte) {
    $problemes[$code] = @{ grave = $grave; texte = $texte }
}

if ($Test) {
    Notifier 'JARVIS — Kynovara' 'Gardien en place : je surveille ton nœud toutes les 5 minutes.'
}

# --- Vérifications -----------------------------------------------------------
$info = $null
$cp = $null
& $docker info --format '{{.ServerVersion}}' *> $null
if ($LASTEXITCODE -ne 0) {
    Probleme 'docker' $true 'Docker Desktop est arrêté : le nœud Kynovara ne tourne pas. Démarre Docker Desktop.'
} else {
    $enMarche = (& $docker inspect -f '{{.State.Running}}' $conteneur 2>$null)
    if ($enMarche -ne 'true') {
        Journal 'conteneur arrêté : redémarrage automatique'
        & $docker start $conteneur *> $null
        Start-Sleep -Seconds 20
        $enMarche = (& $docker inspect -f '{{.State.Running}}' $conteneur 2>$null)
        if ($enMarche -eq 'true') {
            Probleme 'redemarre' $false 'Le nœud Kynovara était arrêté : JARVIS l''a redémarré.'
        } else {
            Probleme 'conteneur' $true 'Le nœud Kynovara est arrêté et ne redémarre pas.'
        }
    }
    if ($enMarche -eq 'true') {
        $info = Rpc '/v1/info'
        $cp = Rpc '/v1/checkpoint'
        if (-not $info) { Probleme 'rpc' $true 'Le nœud Kynovara ne répond pas.' }
    }
}

if ($info) {
    $hauteur = [int64]$info.height
    $age = $maintenant - [int64]$info.tip_time
    if (-not $info.mining.active) { Probleme 'minage' $false 'Le minage est arrêté.' }
    if ($serveurPublic -and [int64]$info.peers.outbound -eq 0) {
        Probleme 'pairs' $false 'Le nœud n''est relié à aucun pair : le lien avec le serveur public est coupé.'
    }
    if ($age -gt $sansBlocMax) {
        Probleme 'blocs' $false ('Aucun nouveau bloc depuis {0} minutes.' -f [math]::Floor($age / 60))
    }
    if ($cp -and $cp.enabled) {
        if ($cp.alert) {
            Probleme 'alerte' $true ('ALERTE points de contrôle : chaîne gelée et paiements suspendus. Décision de la clé maîtresse nécessaire. ' + $cp.alert)
        }
        if (-not $cp.signer) {
            Probleme 'signature' $true 'Le nœud ne signe plus les blocs (clé de signature absente ou révoquée).'
        } else {
            $reste = [int64]$cp.signer.max_height - $hauteur
            if ($reste -le $blocsParJour) {
                Probleme 'echeance' $true ('La clé de signature expire dans {0} blocs (moins d''un jour) : relance « checkpoint delegate ».' -f $reste)
            } elseif ($reste -le 7 * $blocsParJour) {
                Probleme 'echeance' $false ('La clé de signature expire dans environ {0} jours : relance « checkpoint delegate ».' -f [math]::Ceiling($reste / $blocsParJour))
            }
            if ($cp.latest -and ($hauteur - [int64]$cp.latest.height) -gt 3) {
                Probleme 'retard' $false ('Les signatures ont {0} blocs de retard.' -f ($hauteur - [int64]$cp.latest.height))
            }
        }
    }
}

if ($serveurPublic -and -not (PortOuvert $serveurPublic $portPublic)) {
    Probleme 'serveur' $true ('Le serveur public Kynovara ({0}) ne répond pas sur le port {1}.' -f $serveurPublic, $portPublic)
}

# --- Notifications (seulement les changements) -------------------------------
$ancien = $null
if (Test-Path -LiteralPath $etatFichier) {
    try { $ancien = Get-Content -LiteralPath $etatFichier -Raw -Encoding UTF8 | ConvertFrom-Json } catch { $ancien = $null }
}
$anciens = @{}
if ($ancien -and $ancien.problemes) { foreach ($p in $ancien.problemes) { $anciens[[string]$p.code] = $p } }
$derniereGrave = 0
if ($ancien -and $ancien.derniere_notification_grave) { $derniereGrave = [int64]$ancien.derniere_notification_grave }

$nouveaux = @($problemes.Keys | Where-Object { -not $anciens.ContainsKey($_) })
$resolus = @($anciens.Keys | Where-Object { -not $problemes.Contains($_) })
$graves = @($problemes.Keys | Where-Object { $problemes[$_].grave })

foreach ($c in $nouveaux) {
    Notifier 'JARVIS — Kynovara' $problemes[$c].texte
    Journal "PROBLÈME $c : $($problemes[$c].texte)"
    if ($problemes[$c].grave) { $derniereGrave = $maintenant }
}
foreach ($c in $resolus) {
    if ($c -ne 'redemarre') { Notifier 'JARVIS — Kynovara' ('Rétabli : ' + $anciens[$c].texte) }
    Journal "RÉTABLI $c"
}
if ($graves.Count -gt 0 -and $nouveaux.Count -eq 0 -and ($maintenant - $derniereGrave) -ge $rappelGrave) {
    Notifier 'JARVIS — Kynovara (rappel)' (($graves | ForEach-Object { $problemes[$_].texte }) -join ' ')
    $derniereGrave = $maintenant
}

# --- État lisible --------------------------------------------------------------
$etat = [ordered]@{
    verifie_le = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    ok = ($problemes.Count -eq 0)
    hauteur = $(if ($info) { [int64]$info.height } else { $null })
    pairs_sortants = $(if ($info) { [int64]$info.peers.outbound } else { $null })
    minage = $(if ($info) { [bool]$info.mining.active } else { $null })
    point_controle = $(if ($cp -and $cp.latest) { [int64]$cp.latest.height } else { $null })
    signature_jusqu_a = $(if ($cp -and $cp.signer) { [int64]$cp.signer.max_height } else { $null })
    alerte = $(if ($cp) { $cp.alert } else { $null })
    problemes = @($problemes.Keys | ForEach-Object { [ordered]@{ code = $_; grave = $problemes[$_].grave; texte = $problemes[$_].texte } })
    derniere_notification_grave = $derniereGrave
}
$etat | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $etatFichier -Encoding UTF8
