.items[] | select(.active) | {name: .name, score: (.score * 2)}
