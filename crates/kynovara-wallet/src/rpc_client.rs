//! Client de l'interface RPC du nœud (HTTP/1.1 sur 127.0.0.1).

use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;
use zeroize::Zeroizing;

/// Taille maximale d'une réponse. Une adresse de minage utilisée pendant des
/// mois accumule des centaines de milliers de pièces (environ 300 octets de
/// JSON chacune) : la limite doit rester largement au-dessus.
const MAX_RESPONSE: u64 = 512 * 1024 * 1024;

/// Client RPC.
pub struct RpcClient {
    addr: SocketAddr,
    token: Zeroizing<String>,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcClient").field("addr", &self.addr).finish_non_exhaustive()
    }
}

impl RpcClient {
    /// Client utilisant le jeton du fichier `.cookie` du nœud.
    pub fn new(addr: SocketAddr, cookie_path: &Path) -> Result<RpcClient, String> {
        let token = std::fs::read_to_string(cookie_path).map_err(|_| {
            format!(
                "jeton RPC introuvable ({}). Le nœud est-il démarré, avec le même réseau et le même --datadir ?",
                cookie_path.display()
            )
        })?;
        Ok(RpcClient { addr, token: Zeroizing::new(token.trim().to_string()) })
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        self.request("GET", path, None)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        self.request("POST", path, Some(body))
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let unreachable = |e: std::io::Error| {
            format!("le nœud ne répond pas sur {} ({e}). Démarre-le d'abord avec « kynovarad ».", self.addr)
        };
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5)).map_err(unreachable)?;
        stream.set_read_timeout(Some(Duration::from_secs(600))).map_err(unreachable)?;
        stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(unreachable)?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            self.token.as_str(),
            body.len()
        );
        let request = Zeroizing::new(request);
        stream.write_all(request.as_bytes()).map_err(unreachable)?;
        stream.write_all(body.as_bytes()).map_err(unreachable)?;
        let mut raw = Vec::new();
        stream.take(MAX_RESPONSE).read_to_end(&mut raw).map_err(|e| format!("réponse du nœud illisible : {e}"))?;
        let text = String::from_utf8(raw).map_err(|_| "réponse du nœud non UTF-8".to_string())?;
        let (head, body) = text.split_once("\r\n\r\n").ok_or("réponse HTTP incomplète")?;
        let status: u16 = head.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or("statut HTTP illisible")?;
        let value: Value = serde_json::from_str(body).map_err(|e| format!("JSON illisible : {e}"))?;
        if status != 200 {
            let message = value["error"]["message"].as_str().unwrap_or("erreur inconnue");
            return Err(format!("le nœud a refusé la demande : {message}"));
        }
        Ok(value)
    }
}
