import requests

def get_balance(address):
    url = f"https://api.ergoplatform.com/api/v1/addresses/{address}/balance/total"
    response = requests.get(url)
    if response.status_code == 200:
        return response.json()["confirmed"]["nanoErgs"] / 1_000_000_000
    else:
        return None

def get_transactions(address):
    url = f"https://api.ergoplatform.com/api/v1/addresses/{address}/transactions"
    response = requests.get(url)
    if response.status_code == 200:
        return response.json()["total"]
    else:
        return None

def get_last_transaction_details(address):
    url = f"https://api.ergoplatform.com/api/v1/addresses/{address}/transactions"
    response = requests.get(url)
    if response.status_code == 200:
        transactions = response.json().get("items", [])
        for transaction in transactions:
            transaction_id = transaction["id"]
            timestamp = transaction["timestamp"]
            outputs = transaction["outputs"]
            value = sum(output["value"] for output in outputs if output.get("address") == address)
            if value > 0:
                return {
                    "transaction_id": transaction_id,
                    "timestamp": timestamp,
                    "value": value / 1_000_000_000
                }
        return None
    else:
        return None
