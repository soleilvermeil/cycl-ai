//! "Rapport IA": the exact prompt CyclAI would send to the Anthropic API
//! (system prompt + profile + computed data), packaged as one markdown file
//! so it can be pasted into any chat assistant (Claude, ChatGPT…) when no API
//! key is configured.

pub const BILAN_ANSWER_FORMAT: &str = "Réponds directement avec le bilan de la sortie (2-4 phrases, en français), sans introduction ni titre.";

pub const DIGEST_ANSWER_FORMAT: &str = "Réponds en respectant strictement le format de sortie décrit dans la consigne (objet JSON seul). \
     Si tu préfères une réponse lisible, présente la séance en liste (échauffement, blocs, retour au calme, \
     avec durées et watts cibles) suivie de ton bilan qualitatif.";

pub fn render(title: &str, system_prompt: &str, user_message: &str, answer_format: &str) -> String {
    format!(
        "# CyclAI — rapport IA : {title}\n\n\
         > Fichier généré par CyclAI. Colle-le en entier (ou joins-le) dans Claude, ChatGPT ou un autre \
         assistant : il contient la consigne et toutes les données calculées, l'assistant n'a rien d'autre à \
         savoir pour produire la même analyse que l'appel API automatique.\n\n\
         ## Consigne\n\n{system_prompt}\n\n\
         ## Données\n\n{user_message}\n\n\
         ## Réponse attendue\n\n{answer_format}\n",
        system_prompt = system_prompt.trim(),
        user_message = user_message.trim(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_contains_prompt_data_and_answer_format_in_order() {
        let out = render("Sortie du jour", "SYSTEM", "DATA", BILAN_ANSWER_FORMAT);
        let (s, d, a) = (
            out.find("SYSTEM").unwrap(),
            out.find("DATA").unwrap(),
            out.find("Réponse attendue").unwrap(),
        );
        assert!(s < d && d < a);
        assert!(out.starts_with("# CyclAI — rapport IA : Sortie du jour"));
    }
}
